//! Memory Consolidation Service
//!
//! 4-phase periodic consolidation (AutoDream-inspired):
//!   A. Orient   — Group related observations by topic prefix, type, and FTS proximity
//!   B. Gather   — Extract key themes from each group of 3+ related observations
//!   C. Consolidate — LLM call to synthesize each group into a mental model
//!   D. Prune    — Create mental models, reduce source importance, run decay, archive
//!
//! Uses a lightweight (Haiku-class) model to minimize cost.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::ai_provider;
use crate::ai_router::TaskContext;
use crate::database::pg::PgDb;
use crate::memory::importance;

// =============================================================================
// Types
// =============================================================================

/// A raw observation fetched for consolidation with all memory-related fields.
#[derive(Debug, Clone)]
pub struct ConsolidationObservation {
    pub id: i64,
    pub title: String,
    pub content: String,
    pub observation_type: String,
    pub scope: String,
    pub topic_key: Option<String>,
    pub content_hash: String,
    pub revision_count: i32,
    pub duplicate_count: i32,
    pub importance: f64,
    pub access_count: i32,
    pub decay_rate: f64,
    pub is_mental_model: bool,
    pub consolidated_from: Option<Vec<i64>>,
    pub project_id: Option<String>,
    pub workflow_id: Option<String>,
    pub task_run_id: Option<String>,
    pub session_id: Option<String>,
    pub last_accessed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A group of related observations ready for consolidation.
#[derive(Debug, Clone)]
pub struct ObservationGroup {
    pub group_key: String,
    pub reason: GroupingReason,
    pub observations: Vec<ConsolidationObservation>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupingReason {
    TopicPrefix(String),
    SameType(String),
    ContentSimilarity,
}

/// LLM response for a consolidation request.
#[derive(Debug, Deserialize)]
pub struct ConsolidationResult {
    pub title: String,
    pub content: String,
    pub observation_type: String,
    pub keywords: Vec<String>,
    pub supersedes: Vec<i64>,
    #[serde(default)]
    pub contradictions: Option<String>,
}

/// Statistics from a single consolidation run.
///
/// Note: `rename_all = "camelCase"` is required — the MemoryHealthPanel.tsx frontend
/// expects camelCase field names (e.g. `modelsCreated`, not `models_created`).
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsolidationStats {
    pub observations_scanned: i32,
    pub groups_found: i32,
    pub models_created: i32,
    pub observations_merged: i32,
    pub observations_decayed: i32,
    pub observations_archived: i32,
}

/// Configuration for the consolidation service.
#[derive(Debug, Clone)]
pub struct ConsolidationConfig {
    /// Minimum number of observations in a group to trigger consolidation.
    pub min_group_size: usize,
    /// Maximum observations to scan per run.
    pub max_observations: i64,
    /// Retention threshold for decay archival.
    pub archive_threshold: f64,
    /// Model override for consolidation LLM calls (Haiku-class).
    pub model_override: Option<String>,
    /// Provider override for consolidation LLM calls.
    pub provider_override: Option<String>,
    /// Minimum hours between consolidation runs.
    pub cooldown_hours: f64,
}

impl Default for ConsolidationConfig {
    fn default() -> Self {
        Self {
            min_group_size: 3,
            max_observations: 500,
            archive_threshold: super::decay::ARCHIVE_THRESHOLD,
            model_override: None,
            provider_override: None,
            cooldown_hours: 6.0,
        }
    }
}

// =============================================================================
// Phase A: Orient — Group related observations
// =============================================================================

/// Group observations by topic-key prefix, same type, and content overlap.
pub fn group_observations(
    observations: &[ConsolidationObservation],
    min_group_size: usize,
) -> Vec<ObservationGroup> {
    let mut groups: Vec<ObservationGroup> = Vec::new();
    let mut assigned: std::collections::HashSet<i64> = std::collections::HashSet::new();

    // Strategy 1: Group by topic_key prefix (e.g., "auth/", "css/layout/")
    let mut prefix_map: HashMap<String, Vec<&ConsolidationObservation>> = HashMap::new();
    for obs in observations {
        if let Some(ref tk) = obs.topic_key {
            // Use first path segment as prefix
            let prefix = tk.split('/').next().unwrap_or(tk).to_string();
            if !prefix.is_empty() {
                prefix_map.entry(prefix).or_default().push(obs);
            }
        }
    }
    for (prefix, obs_list) in &prefix_map {
        if obs_list.len() >= min_group_size {
            let group_obs: Vec<ConsolidationObservation> =
                obs_list.iter().map(|o| (*o).clone()).collect();
            for o in &group_obs {
                assigned.insert(o.id);
            }
            groups.push(ObservationGroup {
                group_key: format!("topic:{}", prefix),
                reason: GroupingReason::TopicPrefix(prefix.clone()),
                observations: group_obs,
            });
        }
    }

    // Strategy 2: Group by observation_type (only unassigned observations)
    let mut type_map: HashMap<String, Vec<&ConsolidationObservation>> = HashMap::new();
    for obs in observations {
        if !assigned.contains(&obs.id) {
            type_map
                .entry(obs.observation_type.clone())
                .or_default()
                .push(obs);
        }
    }
    for (obs_type, obs_list) in &type_map {
        if obs_list.len() >= min_group_size {
            let group_obs: Vec<ConsolidationObservation> =
                obs_list.iter().map(|o| (*o).clone()).collect();
            for o in &group_obs {
                assigned.insert(o.id);
            }
            groups.push(ObservationGroup {
                group_key: format!("type:{}", obs_type),
                reason: GroupingReason::SameType(obs_type.clone()),
                observations: group_obs,
            });
        }
    }

    // Strategy 3: Content similarity via TF-IDF cosine similarity
    // Extract term vectors from title+content, compute pairwise similarity,
    // and cluster observations that share high similarity.
    let remaining: Vec<&ConsolidationObservation> = observations
        .iter()
        .filter(|o| !assigned.contains(&o.id))
        .collect();

    if remaining.len() >= min_group_size {
        // Build term frequency vectors for each observation
        let term_vecs: Vec<HashMap<String, f64>> = remaining
            .iter()
            .map(|obs| term_frequency(&format!("{} {}", obs.title, obs.content)))
            .collect();

        // Compute IDF across all remaining observations
        let idf = compute_idf(&term_vecs);

        // Compute TF-IDF vectors
        let tfidf_vecs: Vec<HashMap<String, f64>> = term_vecs
            .iter()
            .map(|tf| {
                tf.iter()
                    .map(|(term, freq)| {
                        let idf_val = idf.get(term).copied().unwrap_or(1.0);
                        (term.clone(), freq * idf_val)
                    })
                    .collect()
            })
            .collect();

        // Greedy single-linkage clustering: for each unassigned obs, find the most
        // similar unassigned obs and build clusters above threshold
        let similarity_threshold = 0.25;
        let mut cluster_ids: Vec<Option<usize>> = vec![None; remaining.len()];
        let mut next_cluster = 0usize;

        for i in 0..remaining.len() {
            if assigned.contains(&remaining[i].id) || cluster_ids[i].is_some() {
                continue;
            }
            // Start a new cluster with this observation
            let cluster = next_cluster;
            next_cluster += 1;
            cluster_ids[i] = Some(cluster);

            // Find all similar observations
            for j in (i + 1)..remaining.len() {
                if assigned.contains(&remaining[j].id) || cluster_ids[j].is_some() {
                    continue;
                }
                let sim = tfidf_cosine_similarity(&tfidf_vecs[i], &tfidf_vecs[j]);
                if sim >= similarity_threshold {
                    cluster_ids[j] = Some(cluster);
                }
            }
        }

        // Collect clusters with >= min_group_size members
        let mut cluster_map: HashMap<usize, Vec<usize>> = HashMap::new();
        for (idx, cid) in cluster_ids.iter().enumerate() {
            if let Some(c) = cid {
                cluster_map.entry(*c).or_default().push(idx);
            }
        }

        for (cluster_id, indices) in &cluster_map {
            if indices.len() >= min_group_size {
                let group_obs: Vec<ConsolidationObservation> = indices
                    .iter()
                    .filter(|&&idx| !assigned.contains(&remaining[idx].id))
                    .map(|&idx| remaining[idx].clone())
                    .collect();
                if group_obs.len() >= min_group_size {
                    // Use the most frequent non-stop word as the group key
                    let key_word = find_representative_term(&group_obs);
                    for o in &group_obs {
                        assigned.insert(o.id);
                    }
                    groups.push(ObservationGroup {
                        group_key: format!("similarity:{}:{}", cluster_id, key_word),
                        reason: GroupingReason::ContentSimilarity,
                        observations: group_obs,
                    });
                }
            }
        }
    }

    groups
}

// =============================================================================
// TF-IDF helpers for content similarity grouping
// =============================================================================

const STOP_WORDS: &[&str] = &[
    "the", "and", "for", "that", "this", "with", "from", "are", "was", "were", "been", "have",
    "has", "had", "not", "but", "all", "can", "her", "his", "its", "may", "will", "each", "which",
    "their", "then", "them", "some", "into", "over", "such", "when", "very", "just", "about",
    "also", "more", "other", "than", "only", "should", "could", "would", "after", "before",
];

/// Extract term frequencies from text, filtering stop words and short tokens.
fn term_frequency(text: &str) -> HashMap<String, f64> {
    let mut counts: HashMap<String, f64> = HashMap::new();
    let words: Vec<String> = text
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 2)
        .filter(|w| !STOP_WORDS.contains(w))
        .map(|w| w.to_string())
        .collect();
    let total = words.len().max(1) as f64;
    for word in words {
        *counts.entry(word).or_default() += 1.0;
    }
    for val in counts.values_mut() {
        *val /= total;
    }
    counts
}

/// Compute inverse document frequency across a set of term-frequency vectors.
fn compute_idf(docs: &[HashMap<String, f64>]) -> HashMap<String, f64> {
    let n = docs.len() as f64;
    let mut doc_freq: HashMap<String, f64> = HashMap::new();
    for doc in docs {
        for term in doc.keys() {
            *doc_freq.entry(term.clone()).or_default() += 1.0;
        }
    }
    doc_freq
        .into_iter()
        .map(|(term, df)| (term, (n / df).ln() + 1.0))
        .collect()
}

/// Cosine similarity between two sparse TF-IDF vectors.
fn tfidf_cosine_similarity(a: &HashMap<String, f64>, b: &HashMap<String, f64>) -> f64 {
    let mut dot = 0.0;
    let mut mag_a = 0.0;
    let mut mag_b = 0.0;

    for (term, val_a) in a {
        mag_a += val_a * val_a;
        if let Some(val_b) = b.get(term) {
            dot += val_a * val_b;
        }
    }
    for val_b in b.values() {
        mag_b += val_b * val_b;
    }

    let magnitude = mag_a.sqrt() * mag_b.sqrt();
    if magnitude < f64::EPSILON {
        0.0
    } else {
        dot / magnitude
    }
}

/// Find the most representative (frequent, non-stop) term across a group of observations.
fn find_representative_term(observations: &[ConsolidationObservation]) -> String {
    let mut word_counts: HashMap<String, usize> = HashMap::new();
    for obs in observations {
        let text = format!("{} {}", obs.title, obs.content).to_lowercase();
        let words: std::collections::HashSet<String> = text
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() > 3)
            .filter(|w| !STOP_WORDS.contains(w))
            .map(|w| w.to_string())
            .collect();
        for word in words {
            *word_counts.entry(word).or_default() += 1;
        }
    }
    word_counts
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(word, _)| word)
        .unwrap_or_else(|| "unknown".to_string())
}

// =============================================================================
// Phase B: Gather Signal — Build consolidation prompt
// =============================================================================

/// Build the LLM prompt for consolidating a group of observations.
fn build_consolidation_prompt(group: &ObservationGroup) -> String {
    let mut prompt = String::from(
        "You are consolidating related observations from an autonomous development system.\n\n\
         ## Related Observations (sorted by importance)\n\n",
    );

    // Sort by importance DESC
    let mut sorted: Vec<&ConsolidationObservation> = group.observations.iter().collect();
    sorted.sort_by(|a, b| {
        b.importance
            .partial_cmp(&a.importance)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    for obs in &sorted {
        prompt.push_str(&format!(
            "### {} (id={}, importance={:.2}, type={}, duplicates={}, created={})\n{}\n\n",
            obs.title,
            obs.id,
            obs.importance,
            obs.observation_type,
            obs.duplicate_count,
            obs.created_at.format("%Y-%m-%d"),
            obs.content,
        ));
    }

    prompt.push_str(
        "## Task\n\
         1. Identify the core insight that connects these observations\n\
         2. Note any contradictions or evolution over time\n\
         3. Produce a single \"mental model\" — a consolidated understanding\n\n\
         Return ONLY valid JSON (no markdown fences):\n\
         {\n\
           \"title\": \"...\",\n\
           \"content\": \"...\",  // 2-4 sentences, actionable\n\
           \"observation_type\": \"...\",  // most appropriate type from: decision, architecture, bugfix, pattern, learning, discovery\n\
           \"keywords\": [\"...\"],\n\
           \"supersedes\": [ids],  // which source observation IDs are fully captured\n\
           \"contradictions\": \"...\"  // if any, note them; null if none\n\
         }\n",
    );

    prompt
}

/// Parse the LLM response into a ConsolidationResult.
fn parse_consolidation_response(response: &str) -> Result<ConsolidationResult, String> {
    // Strip markdown fences if present
    let cleaned = response
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    serde_json::from_str(cleaned).map_err(|e| {
        format!(
            "Failed to parse consolidation response: {}. Response: {}",
            e,
            &cleaned[..cleaned.len().min(200)]
        )
    })
}

// =============================================================================
// Phase C & D: Consolidate (LLM call) & Prune
// =============================================================================

/// Run the full consolidation pipeline.
///
/// Returns consolidation stats and any errors encountered.
pub async fn run_consolidation(
    pg: &Arc<PgDb>,
    config: &ConsolidationConfig,
) -> Result<ConsolidationStats, String> {
    let mut stats = ConsolidationStats::default();

    // Insert consolidation log entry
    let log_id = pg
        .insert_consolidation_log()
        .await
        .map_err(|e| format!("Failed to insert consolidation log: {}", e))?;

    let result = run_consolidation_inner(pg, config, &mut stats).await;

    // Complete the log entry
    let error_msg = result.as_ref().err().cloned();
    if let Err(e) = pg
        .complete_consolidation_log(log_id, &stats, error_msg.as_deref())
        .await
    {
        warn!("Failed to complete consolidation log: {}", e);
    }

    // After consolidation (success or failure), invalidate all cached working
    // representations so the next prompt build picks up freshly consolidated data.
    if let Some(wr_cache) =
        crate::memory::working_representation::WorkingRepresentationCache::try_global()
    {
        info!("Invalidating all working representation cache entries after consolidation");
        wr_cache.invalidate_all().await;
    }

    result.map(|_| stats)
}

async fn run_consolidation_inner(
    pg: &Arc<PgDb>,
    config: &ConsolidationConfig,
    stats: &mut ConsolidationStats,
) -> Result<(), String> {
    // Phase A: Orient — fetch and group observations
    info!("Memory consolidation: Phase A — Orient (fetching observations)");
    let observations = pg
        .get_observations_for_consolidation(config.max_observations)
        .await
        .map_err(|e| format!("Failed to fetch observations: {}", e))?;

    stats.observations_scanned = observations.len() as i32;

    if observations.is_empty() {
        info!("Memory consolidation: no observations to consolidate");
        return Ok(());
    }

    // Recompute importance for all fetched observations
    for obs in &observations {
        let new_importance = importance::compute_importance(
            &obs.observation_type,
            obs.duplicate_count,
            obs.revision_count,
            obs.access_count,
            obs.task_run_id.is_some(),
        );
        let new_decay = importance::compute_decay_rate(new_importance, obs.is_mental_model);

        if (new_importance - obs.importance).abs() > 0.01
            || (new_decay - obs.decay_rate).abs() > 0.001
        {
            if let Err(e) = pg
                .update_observation_importance(obs.id, new_importance, new_decay)
                .await
            {
                tracing::warn!(
                    "Failed to update observation importance for id {}: {e}",
                    obs.id
                );
            }
        }
    }

    // Phase A.5: Contradiction Resolution
    info!("Memory consolidation: Phase A.5 — Contradiction resolution");
    match crate::memory::contradiction::run_contradiction_scan(pg, 50).await {
        Ok(cr_stats) => {
            info!(
                "Contradiction resolution: detected={}, auto_resolved={}, failed={}",
                cr_stats.detected, cr_stats.auto_resolved, cr_stats.failed
            );
        }
        Err(e) => warn!("Contradiction resolution failed (non-fatal): {}", e),
    }

    let groups = group_observations(&observations, config.min_group_size);
    stats.groups_found = groups.len() as i32;

    if groups.is_empty() {
        info!(
            "Memory consolidation: no groups of {} or more observations found",
            config.min_group_size
        );
    } else {
        info!(
            "Memory consolidation: Phase B/C — Consolidating {} groups",
            groups.len()
        );
    }

    // Phase B+C: For each group, build prompt and call LLM
    for group in &groups {
        let prompt = build_consolidation_prompt(group);
        let context = TaskContext::from_prompt(&prompt);

        debug!(
            "Consolidating group '{}' ({} observations)",
            group.group_key,
            group.observations.len()
        );

        let model_override = config.model_override.clone();
        let provider_override = config.provider_override.clone();
        let response = tokio::task::spawn_blocking(move || {
            ai_provider::run_prompt_with_model_override(
                &prompt,
                &context,
                None, // doctor_handle
                model_override.as_deref(),
                provider_override.as_deref(),
                Some(0.3), // low temperature for structured output
                Some(1024),
                None, // fallback_model
                None, // fallback_provider
            )
        })
        .await;

        let response = match response {
            Ok(r) => r,
            Err(e) => {
                warn!(
                    "Consolidation spawn_blocking failed for group '{}': {}",
                    group.group_key, e
                );
                continue;
            }
        };

        if !response.success {
            warn!(
                "Consolidation LLM call failed for group '{}': {:?}",
                group.group_key, response.error
            );
            continue;
        }

        let result = match parse_consolidation_response(&response.output) {
            Ok(r) => r,
            Err(e) => {
                warn!(
                    "Failed to parse consolidation for group '{}': {}",
                    group.group_key, e
                );
                continue;
            }
        };

        // Phase D: Prune — create mental model, reduce source importance
        let source_ids: Vec<i64> = group.observations.iter().map(|o| o.id).collect();
        let mental_model_importance = group
            .observations
            .iter()
            .map(|o| o.importance)
            .fold(0.0_f64, f64::max)
            .max(0.7); // mental models start with at least 0.7 importance

        // Pick the broadest scope from source observations (global > project > personal)
        let scope = if group.observations.iter().any(|o| o.scope == "global") {
            "global"
        } else if group.observations.iter().any(|o| o.scope == "project") {
            "project"
        } else {
            "personal"
        };

        let model_id = pg
            .save_mental_model(
                &result.title,
                &result.content,
                &result.observation_type,
                scope,
                mental_model_importance,
                &source_ids,
            )
            .await;

        match model_id {
            Ok(_id) => {
                stats.models_created += 1;

                // Reduce importance of superseded source observations by 50%
                for &obs_id in &result.supersedes {
                    if source_ids.contains(&obs_id) {
                        if let Err(e) = pg.reduce_observation_importance(obs_id, 0.5).await {
                            tracing::warn!(
                                "Failed to reduce observation importance for id {obs_id}: {e}"
                            );
                        }
                        stats.observations_merged += 1;
                    }
                }
            }
            Err(e) => {
                warn!(
                    "Failed to save mental model for group '{}': {}",
                    group.group_key, e
                );
            }
        }
    }

    // Phase D continued: Run decay pass
    info!("Memory consolidation: Phase D — Decay pass");
    let archived_obs = pg
        .decay_and_archive_observations(config.archive_threshold)
        .await
        .unwrap_or(0);
    let archived_models = pg
        .decay_and_archive_mental_models(config.archive_threshold)
        .await
        .unwrap_or(0);

    stats.observations_archived = (archived_obs + archived_models) as i32;
    stats.observations_decayed = stats.observations_archived; // same for now

    info!(
        "Memory consolidation complete: scanned={}, groups={}, models_created={}, merged={}, archived={}",
        stats.observations_scanned,
        stats.groups_found,
        stats.models_created,
        stats.observations_merged,
        stats.observations_archived,
    );

    Ok(())
}

// =============================================================================
// LLM helpers for inductive / abductive reasoning
// =============================================================================

/// LLM response for inductive reasoning.
#[derive(Deserialize)]
struct LlmInductiveResponse {
    conclusion: String,
    confidence: f64,
}

/// LLM response for abductive reasoning.
#[derive(Deserialize)]
struct LlmAbductiveResponse {
    hypothesis: String,
    confidence: f64,
}

/// Strip optional markdown code fences from an LLM response.
fn strip_markdown_fences(raw: &str) -> &str {
    let s = raw.trim();
    if s.starts_with("```") {
        s.trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim()
    } else {
        s
    }
}

/// Call the LLM to induce a general rule from a group of observations.
///
/// Returns `(conclusion, confidence)` on success.
async fn try_llm_inductive(
    obs_type: &str,
    observations: &[&ConsolidationObservation],
) -> Result<(String, f64), String> {
    let count = observations.len();
    let mut obs_lines = String::new();
    for obs in observations.iter().take(10) {
        let preview = if obs.content.len() > 200 {
            format!("{}...", &obs.content[..200])
        } else {
            obs.content.clone()
        };
        obs_lines.push_str(&format!("- {}: {}\n", obs.title, preview));
    }
    if count > 10 {
        obs_lines.push_str(&format!("  ... and {} more\n", count - 10));
    }

    let prompt = format!(
        "These {count} observations of type \"{obs_type}\" share a common pattern:\n\
         {obs_lines}\n\
         What general rule or recurring pattern can be induced from these observations?\n\
         Respond with ONLY valid JSON (no markdown fences, no extra text):\n\
         {{\"conclusion\": \"...\", \"confidence\": 0.0-1.0}}",
        count = count,
        obs_type = obs_type,
        obs_lines = obs_lines,
    );

    let context = TaskContext::from_prompt(&prompt);

    let response = tokio::task::spawn_blocking(move || {
        ai_provider::run_prompt_with_model_override(
            &prompt,
            &context,
            None,
            Some("claude-haiku-4-5-20251001"),
            None,
            Some(0.3),
            Some(512),
            None,
            None,
        )
    })
    .await
    .map_err(|e| format!("spawn_blocking failed: {}", e))?;

    if !response.success {
        return Err(format!(
            "LLM call failed: {}",
            response.error.unwrap_or_default()
        ));
    }

    let json_str = strip_markdown_fences(&response.output);
    let parsed: LlmInductiveResponse = serde_json::from_str(json_str).map_err(|e| {
        format!(
            "Failed to parse inductive JSON: {} — raw: {}",
            e,
            &json_str[..json_str.len().min(200)]
        )
    })?;

    Ok((parsed.conclusion, parsed.confidence.clamp(0.0, 1.0)))
}

/// Call the LLM to hypothesise a common root cause for a cluster of unresolved issues.
///
/// Returns `(hypothesis, confidence)` on success.
async fn try_llm_abductive(
    area: &str,
    cluster: &[&ConsolidationObservation],
) -> Result<(String, f64), String> {
    let count = cluster.len();
    let mut issue_lines = String::new();
    for obs in cluster.iter().take(10) {
        let preview = if obs.content.len() > 200 {
            format!("{}...", &obs.content[..200])
        } else {
            obs.content.clone()
        };
        issue_lines.push_str(&format!("- {}: {}\n", obs.title, preview));
    }
    if count > 10 {
        issue_lines.push_str(&format!("  ... and {} more\n", count - 10));
    }

    let prompt = format!(
        "These {count} unresolved issues in area \"{area}\" suggest a common underlying cause:\n\
         {issue_lines}\n\
         What is the simplest explanation (hypothesis) for these issues occurring together?\n\
         Respond with ONLY valid JSON (no markdown fences, no extra text):\n\
         {{\"hypothesis\": \"...\", \"confidence\": 0.0-1.0}}",
        count = count,
        area = area,
        issue_lines = issue_lines,
    );

    let context = TaskContext::from_prompt(&prompt);

    let response = tokio::task::spawn_blocking(move || {
        ai_provider::run_prompt_with_model_override(
            &prompt,
            &context,
            None,
            Some("claude-haiku-4-5-20251001"),
            None,
            Some(0.3),
            Some(512),
            None,
            None,
        )
    })
    .await
    .map_err(|e| format!("spawn_blocking failed: {}", e))?;

    if !response.success {
        return Err(format!(
            "LLM call failed: {}",
            response.error.unwrap_or_default()
        ));
    }

    let json_str = strip_markdown_fences(&response.output);
    let parsed: LlmAbductiveResponse = serde_json::from_str(json_str).map_err(|e| {
        format!(
            "Failed to parse abductive JSON: {} — raw: {}",
            e,
            &json_str[..json_str.len().min(200)]
        )
    })?;

    Ok((parsed.hypothesis, parsed.confidence.clamp(0.0, 1.0)))
}

// =============================================================================
// Phase E: Inductive Reasoning — detect patterns across multiple observations
// =============================================================================

/// Phase E: Inductive reasoning — detect patterns across multiple observations.
pub(crate) async fn phase_inductive(
    _pg: &Arc<PgDb>,
    observations: &[ConsolidationObservation],
    _config: &ConsolidationConfig,
) -> Result<Vec<crate::database::types::CreateReasoningTraceInput>, String> {
    use crate::database::types::CreateReasoningTraceInput;

    let mut traces = Vec::new();

    // Group observations by observation_type
    let mut by_type: HashMap<String, Vec<&ConsolidationObservation>> = HashMap::new();
    for obs in observations {
        by_type
            .entry(obs.observation_type.clone())
            .or_default()
            .push(obs);
    }

    // For groups with 3+ members, create inductive trace (LLM-enhanced)
    for (obs_type, group) in &by_type {
        if group.len() >= 3 {
            let premise_ids: Vec<i64> = group.iter().map(|o| o.id).collect();
            let titles: Vec<&str> = group.iter().take(5).map(|o| o.title.as_str()).collect();

            // Heuristic fallback values
            let heuristic_conclusion = format!(
                "Recurring {} pattern detected across {} observations: {}",
                obs_type,
                group.len(),
                titles.join(", ")
            );
            let heuristic_confidence = (group.len() as f64 / 10.0).min(0.9);

            // Try LLM, fall back to heuristic
            let (conclusion, confidence, source) = match try_llm_inductive(obs_type, group).await {
                Ok((c, conf)) => {
                    debug!(
                        obs_type,
                        count = group.len(),
                        "LLM inductive reasoning succeeded"
                    );
                    (c, conf, "llm")
                }
                Err(e) => {
                    debug!(
                        obs_type,
                        error = %e,
                        "LLM inductive reasoning unavailable, using heuristic"
                    );
                    (heuristic_conclusion, heuristic_confidence, "heuristic")
                }
            };

            traces.push(CreateReasoningTraceInput {
                reasoning_type: "inductive".to_string(),
                premise_ids,
                conclusion,
                confidence,
                evidence_json: Some(
                    serde_json::json!({
                        "pattern_type": obs_type,
                        "observation_count": group.len(),
                        "sample_titles": titles,
                        "reasoning_source": source,
                    })
                    .to_string(),
                ),
                created_observation_id: None,
                dreamer_run_id: None,
            });
        }
    }

    // Also group by topic_key prefix
    let mut by_prefix: HashMap<String, Vec<&ConsolidationObservation>> = HashMap::new();
    for obs in observations {
        if let Some(ref tk) = obs.topic_key {
            if let Some(prefix) = tk.split('/').next() {
                by_prefix.entry(prefix.to_string()).or_default().push(obs);
            }
        }
    }

    for (prefix, group) in &by_prefix {
        if group.len() >= 3 {
            let premise_ids: Vec<i64> = group.iter().map(|o| o.id).collect();
            let conclusion = format!(
                "Topic area '{}' has {} related observations, suggesting concentrated activity or recurring issues in this domain",
                prefix, group.len()
            );

            traces.push(CreateReasoningTraceInput {
                reasoning_type: "inductive".to_string(),
                premise_ids,
                conclusion,
                confidence: 0.6,
                evidence_json: Some(
                    serde_json::json!({
                        "topic_prefix": prefix,
                        "observation_count": group.len(),
                    })
                    .to_string(),
                ),
                created_observation_id: None,
                dreamer_run_id: None,
            });
        }
    }

    Ok(traces)
}

// =============================================================================
// Phase F: Deductive Reasoning — apply logical rules to existing knowledge
// =============================================================================

/// Phase F: Deductive reasoning — apply logical rules to existing knowledge.
pub(crate) async fn phase_deductive(
    pg: &Arc<PgDb>,
    _config: &ConsolidationConfig,
) -> Result<Vec<crate::database::types::CreateReasoningTraceInput>, String> {
    use crate::database::types::CreateReasoningTraceInput;

    let mut traces = Vec::new();

    // Query: find fixes that resolved findings which later recurred
    let conn = pg
        .pool()
        .get()
        .await
        .map_err(|e| format!("PG pool error: {}", e))?;

    let rows = conn
        .query(
            "SELECT DISTINCT crp.id, crp.pattern_type, crp.signature_hash,
                    crp.occurrence_count, crp.resolved_by_fix_id, crp.pattern_data
             FROM cross_run_patterns crp
             WHERE crp.status = 'active'
               AND crp.resolved_by_fix_id IS NOT NULL
               AND crp.occurrence_count >= 2
             LIMIT 20",
            &[],
        )
        .await
        .map_err(|e| format!("PG deductive query: {}", e))?;

    for row in &rows {
        let pattern_id: String = row.get("id");
        let fix_id: Option<String> = row.get("resolved_by_fix_id");
        let occurrence_count: i32 = row.get("occurrence_count");
        let pattern_type: String = row.get("pattern_type");

        if let Some(ref fix) = fix_id {
            let conclusion = format!(
                "Fix '{}' was applied to resolve pattern '{}', but the pattern recurred {} times. \
                 Deduction: the fix is insufficient and the root cause remains unaddressed.",
                fix, pattern_id, occurrence_count
            );

            traces.push(CreateReasoningTraceInput {
                reasoning_type: "deductive".to_string(),
                premise_ids: vec![], // cross_run_patterns don't have observation IDs
                conclusion,
                confidence: 0.8,
                evidence_json: Some(
                    serde_json::json!({
                        "pattern_id": pattern_id,
                        "pattern_type": pattern_type,
                        "fix_id": fix,
                        "recurrence_count": occurrence_count,
                        "rule": "If fix F resolved pattern P, but P recurred, then F is insufficient"
                    })
                    .to_string(),
                ),
                created_observation_id: None,
                dreamer_run_id: None,
            });
        }
    }

    // Also check mental models that contradict recent observations
    let mental_models = conn
        .query(
            "SELECT id, title, content FROM observations
             WHERE is_mental_model = true AND NOT is_deleted
             ORDER BY importance DESC LIMIT 10",
            &[],
        )
        .await
        .map_err(|e| format!("PG deductive mental models: {}", e))?;

    for model in &mental_models {
        let model_id: i64 = model.get("id");
        let model_title: String = model.get("title");

        // Check if there are recent observations that contradict this model
        let contradictions = conn
            .query(
                "SELECT id, title FROM observations
                 WHERE NOT is_deleted AND NOT is_mental_model
                   AND superseded_by IS NULL
                   AND created_at > NOW() - INTERVAL '7 days'
                   AND topic_key IS NOT NULL
                   AND topic_key IN (SELECT topic_key FROM observations WHERE id = $1)
                   AND content_hash != (SELECT content_hash FROM observations WHERE id = $1)
                 LIMIT 5",
                &[&model_id],
            )
            .await
            .map_err(|e| format!("PG deductive contradiction check: {}", e))?;

        if !contradictions.is_empty() {
            let contra_ids: Vec<i64> = contradictions
                .iter()
                .map(|r| r.get::<_, i64>("id"))
                .collect();
            let conclusion = format!(
                "Mental model '{}' may be outdated: {} recent observations contradict it. \
                 The model should be reviewed and potentially regenerated.",
                model_title,
                contradictions.len()
            );

            let mut premise_ids = vec![model_id];
            premise_ids.extend(&contra_ids);

            let trace_input = CreateReasoningTraceInput {
                reasoning_type: "deductive".to_string(),
                premise_ids,
                conclusion,
                confidence: 0.7,
                evidence_json: Some(
                    serde_json::json!({
                        "mental_model_id": model_id,
                        "contradicting_observation_ids": contra_ids,
                        "rule": "If recent observations contradict a mental model, the model is stale"
                    })
                    .to_string(),
                ),
                created_observation_id: None,
                dreamer_run_id: None,
            };

            // Save immediately so we get the trace ID for invalidation
            match pg.save_reasoning_trace(&trace_input).await {
                Ok(new_trace_id) => {
                    // Invalidate any prior valid traces whose premises relied on
                    // the now-contradicted mental model
                    if let Ok(recent_traces) = pg.get_recent_traces(None, 100).await {
                        for trace in &recent_traces {
                            if trace.is_valid && trace.premise_ids.contains(&model_id) {
                                let _ = pg.invalidate_trace(trace.id, Some(new_trace_id)).await;
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to save deductive trace for contradicted model {}: {}",
                        model_id,
                        e
                    );
                }
            }
            // Still push so the caller's stats.deductive_traces count is accurate.
            // The caller will re-save with dreamer_run_id set; the early save
            // (without dreamer_run_id) was needed to obtain the ID for invalidation.
            traces.push(trace_input);
        }
    }

    Ok(traces)
}

// =============================================================================
// Phase G: Abductive Reasoning — infer simplest explanations
// =============================================================================

/// Phase G: Abductive reasoning — infer simplest explanations for unexplained phenomena.
pub(crate) async fn phase_abductive(
    _pg: &Arc<PgDb>,
    observations: &[ConsolidationObservation],
    _config: &ConsolidationConfig,
) -> Result<Vec<crate::database::types::CreateReasoningTraceInput>, String> {
    use crate::database::types::CreateReasoningTraceInput;

    let mut traces = Vec::new();

    // Find error/bugfix observations that have no linked fixes or resolutions
    let unresolved: Vec<&ConsolidationObservation> = observations
        .iter()
        .filter(|o| {
            (o.observation_type == "bugfix" || o.observation_type == "pattern")
                && o.importance >= 0.3
        })
        .take(20)
        .collect();

    if unresolved.len() >= 2 {
        // Group unresolved by topic_key prefix to find clusters
        let mut clusters: HashMap<String, Vec<&ConsolidationObservation>> = HashMap::new();
        for obs in &unresolved {
            let key = obs
                .topic_key
                .as_deref()
                .and_then(|tk| tk.split('/').next())
                .unwrap_or("unknown")
                .to_string();
            clusters.entry(key).or_default().push(obs);
        }

        for (area, cluster) in &clusters {
            if cluster.len() >= 2 {
                let premise_ids: Vec<i64> = cluster.iter().map(|o| o.id).collect();
                let titles: Vec<&str> = cluster.iter().take(4).map(|o| o.title.as_str()).collect();

                // Heuristic fallback values
                let heuristic_conclusion = format!(
                    "Multiple unresolved issues in area '{}' ({} observations: {}) suggest a common \
                     underlying cause. Hypothesis: there may be a systemic issue in the '{}' domain \
                     that individual fixes are not addressing.",
                    area,
                    cluster.len(),
                    titles.join("; "),
                    area
                );
                let heuristic_confidence = 0.5 + (cluster.len() as f64 * 0.05).min(0.3);

                // Try LLM, fall back to heuristic
                let (conclusion, confidence, source) = match try_llm_abductive(area, cluster).await
                {
                    Ok((hyp, conf)) => {
                        debug!(
                            area,
                            count = cluster.len(),
                            "LLM abductive reasoning succeeded"
                        );
                        (hyp, conf, "llm")
                    }
                    Err(e) => {
                        debug!(
                            area,
                            error = %e,
                            "LLM abductive reasoning unavailable, using heuristic"
                        );
                        (heuristic_conclusion, heuristic_confidence, "heuristic")
                    }
                };

                traces.push(CreateReasoningTraceInput {
                    reasoning_type: "abductive".to_string(),
                    premise_ids,
                    conclusion,
                    confidence,
                    evidence_json: Some(
                        serde_json::json!({
                            "area": area,
                            "unresolved_count": cluster.len(),
                            "sample_titles": titles,
                            "reasoning_source": source,
                        })
                        .to_string(),
                    ),
                    created_observation_id: None,
                    dreamer_run_id: None,
                });
            }
        }
    }

    Ok(traces)
}

// =============================================================================
// Dreamer Orchestrator — standard consolidation + formal reasoning phases
// =============================================================================

/// Run the full dreamer cycle: standard consolidation + formal reasoning phases.
pub async fn run_dreamer(
    pg: &Arc<PgDb>,
    config: &ConsolidationConfig,
) -> Result<crate::database::types::DreamerStats, String> {
    use crate::database::types::DreamerStats;

    let mut stats = DreamerStats::default();

    // Insert dreamer log entry
    let log_id = pg.insert_dreamer_log().await?;

    info!(
        "Dreamer: starting formal reasoning cycle (log_id={})",
        log_id
    );

    // First, run standard consolidation
    match run_consolidation(pg, config).await {
        Ok(cs) => info!(
            "Dreamer: consolidation complete (models={}, merged={})",
            cs.models_created, cs.observations_merged
        ),
        Err(e) => warn!("Dreamer: consolidation failed (non-fatal): {}", e),
    }

    // Run all reasoning phases, ensuring we always complete the dreamer log
    let result: Result<DreamerStats, String> = async {
        // Fetch observations for reasoning
        let observations = pg
            .get_observations_for_consolidation(config.max_observations)
            .await?;

        // Phase E: Inductive reasoning
        info!("Dreamer: Phase E — Inductive reasoning");
        match phase_inductive(pg, &observations, config).await {
            Ok(traces) => {
                stats.inductive_traces = traces.len() as i32;
                for mut trace in traces {
                    trace.dreamer_run_id = Some(log_id);
                    if let Err(e) = pg.save_reasoning_trace(&trace).await {
                        warn!("Dreamer: failed to save inductive trace: {}", e);
                    }
                }
            }
            Err(e) => warn!("Dreamer: inductive reasoning failed (non-fatal): {}", e),
        }

        // Phase F: Deductive reasoning
        info!("Dreamer: Phase F — Deductive reasoning");
        match phase_deductive(pg, config).await {
            Ok(traces) => {
                stats.deductive_traces = traces.len() as i32;
                for mut trace in traces {
                    trace.dreamer_run_id = Some(log_id);
                    if let Err(e) = pg.save_reasoning_trace(&trace).await {
                        warn!("Dreamer: failed to save deductive trace: {}", e);
                    }
                }
            }
            Err(e) => warn!("Dreamer: deductive reasoning failed (non-fatal): {}", e),
        }

        // Phase G: Abductive reasoning
        info!("Dreamer: Phase G — Abductive reasoning");
        match phase_abductive(pg, &observations, config).await {
            Ok(traces) => {
                stats.abductive_traces = traces.len() as i32;
                for mut trace in traces {
                    trace.dreamer_run_id = Some(log_id);
                    // Create observation for high-confidence abductive hypotheses
                    if trace.confidence > 0.7 {
                        let obs_input = crate::database::types::CreateObservationInput {
                            title: format!(
                                "Hypothesis: {}",
                                &trace.conclusion[..trace.conclusion.len().min(80)]
                            ),
                            content: trace.conclusion.clone(),
                            observation_type: "hypothesis".to_string(),
                            scope: "project".to_string(),
                            topic_key: None,
                            project_id: None,
                            workflow_id: None,
                            task_run_id: None,
                            session_id: None,
                        };
                        match pg.save_observation(&obs_input).await {
                            Ok(obs_id) => {
                                trace.created_observation_id = Some(obs_id);
                                stats.observations_created += 1;
                            }
                            Err(e) => {
                                warn!("Dreamer: failed to save hypothesis observation: {}", e)
                            }
                        }
                    }
                    if let Err(e) = pg.save_reasoning_trace(&trace).await {
                        warn!("Dreamer: failed to save abductive trace: {}", e);
                    }
                }
            }
            Err(e) => warn!("Dreamer: abductive reasoning failed (non-fatal): {}", e),
        }

        Ok(stats.clone())
    }
    .await;

    // Always complete the dreamer log, even on error
    match &result {
        Ok(s) => {
            let _ = pg.complete_dreamer_log(log_id, s, None).await;
        }
        Err(e) => {
            let empty = DreamerStats::default();
            let _ = pg
                .complete_dreamer_log(log_id, &empty, Some(e.as_str()))
                .await;
        }
    }

    let stats = result?;

    info!(
        "Dreamer complete: inductive={}, deductive={}, abductive={}, observations_created={}",
        stats.inductive_traces,
        stats.deductive_traces,
        stats.abductive_traces,
        stats.observations_created
    );

    Ok(stats)
}

/// Check if consolidation is allowed (cooldown enforcement).
pub async fn can_run_consolidation(pg: &Arc<PgDb>, cooldown_hours: f64) -> bool {
    match pg.get_last_consolidation_time().await {
        Ok(Some(last_run)) => {
            let elapsed = Utc::now().signed_duration_since(last_run);
            let cooldown_secs = (cooldown_hours * 3600.0) as i64;
            elapsed.num_seconds() >= cooldown_secs
        }
        Ok(None) => true, // never run before
        Err(_) => true,   // error reading — allow run
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_obs(
        id: i64,
        title: &str,
        obs_type: &str,
        topic_key: Option<&str>,
        importance: f64,
    ) -> ConsolidationObservation {
        ConsolidationObservation {
            id,
            title: title.to_string(),
            content: format!("Content for {}", title),
            observation_type: obs_type.to_string(),
            scope: "project".to_string(),
            topic_key: topic_key.map(|s| s.to_string()),
            content_hash: format!("hash_{}", id),
            revision_count: 1,
            duplicate_count: 0,
            importance,
            access_count: 0,
            decay_rate: 0.1,
            is_mental_model: false,
            consolidated_from: None,
            project_id: None,
            workflow_id: None,
            task_run_id: None,
            session_id: None,
            last_accessed_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn test_group_by_topic_prefix() {
        let obs = vec![
            make_obs(
                1,
                "Auth token expiry",
                "bugfix",
                Some("auth/token-expiry"),
                0.6,
            ),
            make_obs(
                2,
                "Auth middleware update",
                "architecture",
                Some("auth/middleware"),
                0.8,
            ),
            make_obs(
                3,
                "Auth session handling",
                "pattern",
                Some("auth/sessions"),
                0.5,
            ),
            make_obs(4, "CSS grid layout fix", "bugfix", Some("css/grid"), 0.5),
        ];

        let groups = group_observations(&obs, 3);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].group_key, "topic:auth");
        assert_eq!(groups[0].observations.len(), 3);
    }

    #[test]
    fn test_group_by_type() {
        let obs = vec![
            make_obs(1, "Fix login crash", "bugfix", None, 0.6),
            make_obs(2, "Fix signup validation", "bugfix", None, 0.5),
            make_obs(3, "Fix password reset", "bugfix", None, 0.5),
            make_obs(4, "New caching layer", "architecture", None, 0.8),
        ];

        let groups = group_observations(&obs, 3);
        assert_eq!(groups.len(), 1);
        assert!(groups[0].group_key.starts_with("type:bugfix"));
        assert_eq!(groups[0].observations.len(), 3);
    }

    #[test]
    fn test_no_groups_below_min_size() {
        let obs = vec![
            make_obs(1, "Single observation A", "bugfix", None, 0.6),
            make_obs(2, "Single observation B", "pattern", None, 0.5),
        ];

        let groups = group_observations(&obs, 3);
        assert!(groups.is_empty());
    }

    #[test]
    fn test_multiple_grouping_strategies() {
        let obs = vec![
            // 3 auth observations → topic group
            make_obs(1, "Auth token", "bugfix", Some("auth/token"), 0.6),
            make_obs(2, "Auth session", "bugfix", Some("auth/session"), 0.6),
            make_obs(3, "Auth middleware", "architecture", Some("auth/mw"), 0.8),
            // 3 pattern observations (no topic key) → type group
            make_obs(4, "Pattern A", "pattern", None, 0.5),
            make_obs(5, "Pattern B", "pattern", None, 0.5),
            make_obs(6, "Pattern C", "pattern", None, 0.5),
        ];

        let groups = group_observations(&obs, 3);
        assert_eq!(groups.len(), 2);
    }

    #[test]
    fn test_parse_consolidation_response_valid() {
        let json = r#"{
            "title": "Auth Token Management",
            "content": "Authentication tokens expire after 1 hour. Refresh must happen before expiry.",
            "observation_type": "architecture",
            "keywords": ["auth", "token", "refresh"],
            "supersedes": [1, 2, 3],
            "contradictions": null
        }"#;

        let result = parse_consolidation_response(json).unwrap();
        assert_eq!(result.title, "Auth Token Management");
        assert_eq!(result.supersedes, vec![1, 2, 3]);
    }

    #[test]
    fn test_parse_consolidation_response_with_fences() {
        let json = "```json\n{\"title\": \"Test\", \"content\": \"Test content\", \"observation_type\": \"pattern\", \"keywords\": [], \"supersedes\": []}\n```";
        let result = parse_consolidation_response(json).unwrap();
        assert_eq!(result.title, "Test");
    }

    #[test]
    fn test_parse_consolidation_response_invalid() {
        let result = parse_consolidation_response("not json");
        assert!(result.is_err());
    }

    #[test]
    fn test_build_consolidation_prompt() {
        let group = ObservationGroup {
            group_key: "topic:auth".to_string(),
            reason: GroupingReason::TopicPrefix("auth".to_string()),
            observations: vec![
                make_obs(1, "Auth token expiry", "bugfix", Some("auth/token"), 0.6),
                make_obs(
                    2,
                    "Auth session handling",
                    "pattern",
                    Some("auth/session"),
                    0.5,
                ),
                make_obs(
                    3,
                    "Auth middleware update",
                    "architecture",
                    Some("auth/mw"),
                    0.8,
                ),
            ],
        };

        let prompt = build_consolidation_prompt(&group);
        assert!(prompt.contains("Auth token expiry"));
        assert!(prompt.contains("Auth middleware update"));
        assert!(prompt.contains("mental model"));
        assert!(prompt.contains("Return ONLY valid JSON"));
    }

    #[test]
    fn test_build_prompt_sorts_by_importance() {
        let group = ObservationGroup {
            group_key: "test".to_string(),
            reason: GroupingReason::SameType("bugfix".to_string()),
            observations: vec![
                make_obs(1, "Low importance", "bugfix", None, 0.3),
                make_obs(2, "High importance", "bugfix", None, 0.9),
                make_obs(3, "Medium importance", "bugfix", None, 0.5),
            ],
        };

        let prompt = build_consolidation_prompt(&group);
        // High importance should appear before low importance
        let high_pos = prompt.find("High importance").unwrap();
        let low_pos = prompt.find("Low importance").unwrap();
        assert!(high_pos < low_pos);
    }

    #[test]
    fn test_group_keyword_overlap() {
        let obs = vec![
            make_obs(1, "Database connection timeout error", "bugfix", None, 0.5),
            make_obs(2, "Database migration timeout issue", "bugfix", None, 0.5),
            make_obs(3, "Database query timeout fix", "bugfix", None, 0.5),
        ];

        // These should group by keyword overlap ("database", "timeout")
        let groups = group_observations(&obs, 3);
        // The 3 bugfix types should form a type group
        assert!(!groups.is_empty());
    }

    #[test]
    fn test_empty_observations_no_groups() {
        let groups = group_observations(&[], 3);
        assert!(groups.is_empty());
    }

    #[test]
    fn test_parse_response_with_contradictions() {
        let json = r#"{
            "title": "Test",
            "content": "Test content",
            "observation_type": "pattern",
            "keywords": ["a"],
            "supersedes": [1],
            "contradictions": "Observation 1 says X but observation 2 says Y"
        }"#;

        let result = parse_consolidation_response(json).unwrap();
        assert_eq!(
            result.contradictions.as_deref(),
            Some("Observation 1 says X but observation 2 says Y")
        );
    }

    #[test]
    fn test_consolidation_config_defaults() {
        let config = ConsolidationConfig::default();
        assert_eq!(config.min_group_size, 3);
        assert_eq!(config.max_observations, 500);
        assert!((config.archive_threshold - 0.05).abs() < f64::EPSILON);
        assert!((config.cooldown_hours - 6.0).abs() < f64::EPSILON);
        assert!(config.model_override.is_none());
    }

    #[test]
    fn test_observations_not_double_assigned() {
        // Observations assigned to a topic group should not also appear in a type group
        let obs = vec![
            make_obs(1, "Auth A", "bugfix", Some("auth/a"), 0.6),
            make_obs(2, "Auth B", "bugfix", Some("auth/b"), 0.6),
            make_obs(3, "Auth C", "bugfix", Some("auth/c"), 0.6),
            make_obs(4, "Fix D", "bugfix", None, 0.5),
        ];

        let groups = group_observations(&obs, 3);
        // Only one group (topic:auth). The remaining bugfix (id=4) alone doesn't form a group.
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].group_key, "topic:auth");
    }

    // TF-IDF helper tests
    #[test]
    fn test_term_frequency() {
        let tf = term_frequency("database connection timeout database error");
        assert!(tf.contains_key("database"));
        assert!(tf.contains_key("connection"));
        assert!(tf.contains_key("timeout"));
        assert!(tf.contains_key("error"));
        // "database" appears twice, should have higher frequency
        assert!(tf["database"] > tf["connection"]);
    }

    #[test]
    fn test_term_frequency_filters_stop_words() {
        let tf = term_frequency("the quick brown fox and the lazy dog");
        assert!(!tf.contains_key("the"));
        assert!(!tf.contains_key("and"));
        assert!(tf.contains_key("quick"));
        assert!(tf.contains_key("brown"));
    }

    #[test]
    fn test_tfidf_cosine_identical() {
        let a: HashMap<String, f64> =
            [("auth".to_string(), 0.5), ("token".to_string(), 0.3)].into();
        let b = a.clone();
        let sim = tfidf_cosine_similarity(&a, &b);
        assert!((sim - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_tfidf_cosine_orthogonal() {
        let a: HashMap<String, f64> = [("auth".to_string(), 1.0)].into();
        let b: HashMap<String, f64> = [("database".to_string(), 1.0)].into();
        let sim = tfidf_cosine_similarity(&a, &b);
        assert!(sim.abs() < f64::EPSILON);
    }

    #[test]
    fn test_tfidf_cosine_partial_overlap() {
        let a: HashMap<String, f64> =
            [("auth".to_string(), 0.5), ("token".to_string(), 0.5)].into();
        let b: HashMap<String, f64> =
            [("auth".to_string(), 0.5), ("session".to_string(), 0.5)].into();
        let sim = tfidf_cosine_similarity(&a, &b);
        assert!(sim > 0.0);
        assert!(sim < 1.0);
    }

    #[test]
    fn test_similarity_grouping_clusters_related_content() {
        let obs = vec![
            make_obs(
                1,
                "Database connection pool timeout error handling",
                "bugfix",
                None,
                0.5,
            ),
            make_obs(
                2,
                "Database connection retry timeout strategy",
                "bugfix",
                None,
                0.5,
            ),
            make_obs(
                3,
                "Database pool connection error recovery",
                "bugfix",
                None,
                0.5,
            ),
            make_obs(4, "CSS grid layout alignment issue", "bugfix", None, 0.5),
        ];

        let groups = group_observations(&obs, 3);
        // The 4 bugfix types form a type group; or the 3 database ones cluster by similarity
        assert!(!groups.is_empty());
    }
}
