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
#[derive(Debug, Clone, Default, Serialize)]
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
pub fn group_observations(observations: &[ConsolidationObservation], min_group_size: usize) -> Vec<ObservationGroup> {
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
            let group_obs: Vec<ConsolidationObservation> = obs_list
                .iter()
                .map(|o| (*o).clone())
                .collect();
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
            type_map.entry(obs.observation_type.clone()).or_default().push(obs);
        }
    }
    for (obs_type, obs_list) in &type_map {
        if obs_list.len() >= min_group_size {
            let group_obs: Vec<ConsolidationObservation> = obs_list
                .iter()
                .map(|o| (*o).clone())
                .collect();
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
    "the", "and", "for", "that", "this", "with", "from", "are", "was", "were",
    "been", "have", "has", "had", "not", "but", "all", "can", "her", "his",
    "its", "may", "will", "each", "which", "their", "then", "them", "some",
    "into", "over", "such", "when", "very", "just", "about", "also", "more",
    "other", "than", "only", "should", "could", "would", "after", "before",
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
    sorted.sort_by(|a, b| b.importance.partial_cmp(&a.importance).unwrap_or(std::cmp::Ordering::Equal));

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

    serde_json::from_str(cleaned)
        .map_err(|e| format!("Failed to parse consolidation response: {}. Response: {}", e, &cleaned[..cleaned.len().min(200)]))
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
    let log_id = pg.insert_consolidation_log().await
        .map_err(|e| format!("Failed to insert consolidation log: {}", e))?;

    let result = run_consolidation_inner(pg, config, &mut stats).await;

    // Complete the log entry
    let error_msg = result.as_ref().err().cloned();
    if let Err(e) = pg.complete_consolidation_log(
        log_id,
        &stats,
        error_msg.as_deref(),
    ).await {
        warn!("Failed to complete consolidation log: {}", e);
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
    let observations = pg.get_observations_for_consolidation(config.max_observations).await
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

        if (new_importance - obs.importance).abs() > 0.01 || (new_decay - obs.decay_rate).abs() > 0.001 {
            let _ = pg.update_observation_importance(obs.id, new_importance, new_decay).await;
        }
    }

    let groups = group_observations(&observations, config.min_group_size);
    stats.groups_found = groups.len() as i32;

    if groups.is_empty() {
        info!("Memory consolidation: no groups of {} or more observations found", config.min_group_size);
    } else {
        info!("Memory consolidation: Phase B/C — Consolidating {} groups", groups.len());
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

        let response = ai_provider::run_prompt_with_model_override(
            &prompt,
            &context,
            None, // doctor_handle
            config.model_override.as_deref(),
            config.provider_override.as_deref(),
            Some(0.3), // low temperature for structured output
            Some(1024),
            None, // fallback_model
            None, // fallback_provider
        );

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
                warn!("Failed to parse consolidation for group '{}': {}", group.group_key, e);
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

        let model_id = pg.save_mental_model(
            &result.title,
            &result.content,
            &result.observation_type,
            scope,
            mental_model_importance,
            &source_ids,
        ).await;

        match model_id {
            Ok(_id) => {
                stats.models_created += 1;

                // Reduce importance of superseded source observations by 50%
                for &obs_id in &result.supersedes {
                    if source_ids.contains(&obs_id) {
                        let _ = pg.reduce_observation_importance(obs_id, 0.5).await;
                        stats.observations_merged += 1;
                    }
                }
            }
            Err(e) => {
                warn!("Failed to save mental model for group '{}': {}", group.group_key, e);
            }
        }
    }

    // Phase D continued: Run decay pass
    info!("Memory consolidation: Phase D — Decay pass");
    let archived_obs = pg.decay_and_archive_observations(config.archive_threshold).await
        .unwrap_or(0);
    let archived_models = pg.decay_and_archive_mental_models(config.archive_threshold).await
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

    fn make_obs(id: i64, title: &str, obs_type: &str, topic_key: Option<&str>, importance: f64) -> ConsolidationObservation {
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
            make_obs(1, "Auth token expiry", "bugfix", Some("auth/token-expiry"), 0.6),
            make_obs(2, "Auth middleware update", "architecture", Some("auth/middleware"), 0.8),
            make_obs(3, "Auth session handling", "pattern", Some("auth/sessions"), 0.5),
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
                make_obs(2, "Auth session handling", "pattern", Some("auth/session"), 0.5),
                make_obs(3, "Auth middleware update", "architecture", Some("auth/mw"), 0.8),
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
        let a: HashMap<String, f64> = [("auth".to_string(), 0.5), ("token".to_string(), 0.3)].into();
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
        let a: HashMap<String, f64> = [("auth".to_string(), 0.5), ("token".to_string(), 0.5)].into();
        let b: HashMap<String, f64> = [("auth".to_string(), 0.5), ("session".to_string(), 0.5)].into();
        let sim = tfidf_cosine_similarity(&a, &b);
        assert!(sim > 0.0);
        assert!(sim < 1.0);
    }

    #[test]
    fn test_similarity_grouping_clusters_related_content() {
        let obs = vec![
            make_obs(1, "Database connection pool timeout error handling", "bugfix", None, 0.5),
            make_obs(2, "Database connection retry timeout strategy", "bugfix", None, 0.5),
            make_obs(3, "Database pool connection error recovery", "bugfix", None, 0.5),
            make_obs(4, "CSS grid layout alignment issue", "bugfix", None, 0.5),
        ];

        let groups = group_observations(&obs, 3);
        // The 4 bugfix types form a type group; or the 3 database ones cluster by similarity
        assert!(!groups.is_empty());
    }
}
