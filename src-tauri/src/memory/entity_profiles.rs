//! Entity profile generation logic.
//!
//! Gathers evidence from observations, findings, and patterns to build
//! rich "Peer Card" style entity profiles using LLM synthesis.
//! Falls back to heuristic summaries when no AI provider is available.

use serde::Deserialize;
use tracing::{debug, info, warn};

use crate::ai_provider;
use crate::ai_router::TaskContext;
use crate::database::pg::PgDb;
use crate::database::types::{CreateEntityProfileInput, ObservationSearchResult};

/// Parsed LLM profile response.
#[derive(Debug, Deserialize)]
struct LlmProfileResponse {
    summary: String,
    detail: String,
}

/// Generate (or refresh) an entity profile by gathering evidence from the database.
///
/// Searches observations by entity label, counts findings/patterns, and builds
/// a heuristic summary string. The result is upserted into entity_profiles.
///
/// Returns the profile ID.
pub async fn generate_entity_profile(
    pg: &PgDb,
    entity_kind: &str,
    entity_id: &str,
    entity_label: &str,
) -> Result<i64, String> {
    // 1. Search observations mentioning this entity
    let observations = pg
        .search_observations(entity_label, None, 50)
        .await
        .unwrap_or_else(|e| {
            warn!("Failed to search observations for entity profile: {}", e);
            vec![]
        });

    let obs_count = observations.len();
    let obs_ids: Vec<i64> = observations.iter().map(|o| o.id).collect();

    // 2. Gather observation types for topic extraction
    let mut topic_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for obs in &observations {
        if let Some(ref tk) = obs.topic_key {
            *topic_counts.entry(tk.clone()).or_insert(0) += 1;
        }
        *topic_counts.entry(obs.observation_type.clone()).or_insert(0) += 1;
    }

    // Sort topics by frequency, take top 5
    let mut topics: Vec<(String, usize)> = topic_counts.into_iter().collect();
    topics.sort_by(|a, b| b.1.cmp(&a.1));
    let top_topics: Vec<String> = topics.iter().take(5).map(|(t, _)| t.clone()).collect();

    // 3. Search findings related to this entity (with title + status for LLM context)
    let conn = pg
        .pool()
        .get()
        .await
        .map_err(|e| format!("PG pool error: {}", e))?;

    let finding_rows = conn
        .query(
            "SELECT id, title, status, COALESCE(LEFT(description, 200), '') as desc_preview
             FROM task_run_findings
             WHERE (title ILIKE '%' || $1 || '%' OR description ILIKE '%' || $1 || '%')
             LIMIT 100",
            &[&entity_label],
        )
        .await
        .unwrap_or_else(|e| {
            warn!("Failed to query findings for entity profile: {}", e);
            vec![]
        });

    let finding_count = finding_rows.len();
    let finding_ids: Vec<String> = finding_rows
        .iter()
        .map(|r| {
            let id: String = r.get(0);
            id
        })
        .collect();

    // Collect finding details for LLM prompt
    let finding_details: Vec<(String, String)> = finding_rows
        .iter()
        .map(|r| {
            let title: String = r.get(1);
            let status: String = r.get(2);
            (title, status)
        })
        .collect();

    let resolved_count = finding_details
        .iter()
        .filter(|(_, status)| status == "resolved" || status == "fixed")
        .count();

    // 4. Search cross-run patterns (with pattern_data for LLM context)
    let pattern_rows = conn
        .query(
            "SELECT id, COALESCE(LEFT(pattern_data, 300), '') as pattern_preview
             FROM cross_run_patterns
             WHERE pattern_data ILIKE '%' || $1 || '%'
                OR affected_components ILIKE '%' || $1 || '%'
             LIMIT 50",
            &[&entity_label],
        )
        .await
        .unwrap_or_else(|e| {
            warn!("Failed to query patterns for entity profile: {}", e);
            vec![]
        });

    let pattern_ids: Vec<String> = pattern_rows
        .iter()
        .map(|r| {
            let id: String = r.get(0);
            id
        })
        .collect();

    let pattern_previews: Vec<String> = pattern_rows
        .iter()
        .map(|r| {
            let preview: String = r.get(1);
            preview
        })
        .collect();

    // 5. Determine last active date
    let last_active = observations
        .iter()
        .map(|o| o.updated_at.as_str())
        .max()
        .unwrap_or("unknown")
        .to_string();

    // 6. Build profile via LLM with heuristic fallback
    let topics_str = if top_topics.is_empty() {
        "none identified".to_string()
    } else {
        top_topics.join(", ")
    };

    let (profile_summary, profile_detail) = match generate_profile_via_llm(
        entity_kind,
        entity_id,
        entity_label,
        &observations,
        &finding_details,
        &pattern_previews,
    )
    .await
    {
        Ok((summary, detail)) => {
            debug!(
                entity_label,
                "Generated LLM-based entity profile"
            );
            (summary, Some(detail))
        }
        Err(e) => {
            warn!("LLM profile generation failed, using heuristic: {}", e);
            generate_heuristic_profile(
                entity_kind,
                entity_id,
                entity_label,
                obs_count,
                finding_count,
                resolved_count,
                &topics_str,
                &last_active,
                &pattern_ids,
            )
        }
    };

    // 8. Upsert
    let input = CreateEntityProfileInput {
        entity_kind: entity_kind.to_string(),
        entity_id: entity_id.to_string(),
        entity_label: entity_label.to_string(),
        profile_summary,
        profile_detail,
        source_observation_ids: if obs_ids.is_empty() { None } else { Some(obs_ids) },
        source_finding_ids: if finding_ids.is_empty() { None } else { Some(finding_ids) },
        source_fix_ids: None,
        source_cross_run_pattern_ids: if pattern_ids.is_empty() { None } else { Some(pattern_ids) },
    };

    let id = pg.save_entity_profile(&input).await?;
    info!(
        entity_kind,
        entity_id,
        entity_label,
        id,
        "Generated entity profile"
    );

    Ok(id)
}

/// Refresh all stale entity profiles.
///
/// Fetches profiles older than `stale_days`, regenerates each by gathering
/// fresh evidence from observations/findings/patterns.
///
/// Returns the number of profiles refreshed.
pub async fn refresh_stale_profiles(
    pg: &PgDb,
    stale_days: i32,
    max_profiles: i64,
) -> Result<usize, String> {
    let stale = pg.get_stale_profiles(stale_days, max_profiles).await?;
    let count = stale.len();

    for profile in &stale {
        if let Err(e) = generate_entity_profile(
            pg,
            &profile.entity_kind,
            &profile.entity_id,
            &profile.entity_label,
        )
        .await
        {
            warn!(
                entity_kind = %profile.entity_kind,
                entity_id = %profile.entity_id,
                error = %e,
                "Failed to refresh stale entity profile"
            );
        }
    }

    info!(count, stale_days, "Refreshed stale entity profiles");
    Ok(count)
}

// =============================================================================
// LLM-based profile generation
// =============================================================================

/// Build a prompt and call the AI provider to generate a rich narrative profile.
///
/// Uses a Haiku-class model for cost efficiency. Returns `(summary, detail)`.
async fn generate_profile_via_llm(
    entity_kind: &str,
    entity_id: &str,
    entity_label: &str,
    observations: &[ObservationSearchResult],
    findings: &[(String, String)],
    pattern_previews: &[String],
) -> Result<(String, String), String> {
    // Build observations section (limit to first 20 for prompt size)
    let mut obs_section = String::new();
    for obs in observations.iter().take(20) {
        obs_section.push_str(&format!(
            "- [{}] {}: {}\n",
            obs.observation_type, obs.title, obs.content_preview
        ));
    }
    if observations.len() > 20 {
        obs_section.push_str(&format!("  ... and {} more\n", observations.len() - 20));
    }
    if obs_section.is_empty() {
        obs_section.push_str("  (none)\n");
    }

    // Build findings section (limit to first 20)
    let mut findings_section = String::new();
    for (title, status) in findings.iter().take(20) {
        findings_section.push_str(&format!("- [{}] {}\n", status, title));
    }
    if findings.len() > 20 {
        findings_section.push_str(&format!("  ... and {} more\n", findings.len() - 20));
    }
    if findings_section.is_empty() {
        findings_section.push_str("  (none)\n");
    }

    // Build patterns section (limit to first 10)
    let mut patterns_section = String::new();
    for preview in pattern_previews.iter().take(10) {
        patterns_section.push_str(&format!("- {}\n", preview));
    }
    if pattern_previews.len() > 10 {
        patterns_section.push_str(&format!(
            "  ... and {} more\n",
            pattern_previews.len() - 10
        ));
    }
    if patterns_section.is_empty() {
        patterns_section.push_str("  (none)\n");
    }

    let prompt = format!(
        "You are analyzing an entity in a software automation system. Generate a concise biographical profile.\n\n\
         Entity: {entity_label} (type: {entity_kind}, id: {entity_id})\n\n\
         Evidence:\n\
         ## Observations ({obs_count})\n\
         {obs_section}\n\
         ## Findings ({finding_count})\n\
         {findings_section}\n\
         ## Cross-Run Patterns ({pattern_count})\n\
         {patterns_section}\n\
         Generate a profile with:\n\
         1. A 2-3 sentence summary of this entity's role and history\n\
         2. Key characteristics (stability, common issues, interaction patterns)\n\
         3. Any recommendations for working with this entity\n\n\
         Return ONLY valid JSON (no markdown fences):\n\
         {{\"summary\": \"...\", \"detail\": \"...\"}}\n",
        entity_label = entity_label,
        entity_kind = entity_kind,
        entity_id = entity_id,
        obs_count = observations.len(),
        obs_section = obs_section,
        finding_count = findings.len(),
        findings_section = findings_section,
        pattern_count = pattern_previews.len(),
        patterns_section = patterns_section,
    );

    let context = TaskContext::from_prompt(&prompt);

    // Call the LLM via spawn_blocking (same pattern as consolidation.rs)
    let prompt_clone = prompt.clone();
    let response = tokio::task::spawn_blocking(move || {
        ai_provider::run_prompt_with_model_override(
            &prompt_clone,
            &context,
            None, // doctor_handle
            None, // model_override — uses Haiku-class via task router
            None, // provider_override
            Some(0.3), // low temperature for structured output
            Some(1024),
            None, // fallback_model
            None, // fallback_provider
        )
    })
    .await
    .map_err(|e| format!("spawn_blocking failed: {}", e))?;

    if !response.success {
        return Err(format!(
            "LLM call failed: {:?}",
            response.error.unwrap_or_else(|| "unknown error".to_string())
        ));
    }

    // Parse the JSON response
    let cleaned = response
        .output
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    let parsed: LlmProfileResponse = serde_json::from_str(cleaned).map_err(|e| {
        format!(
            "Failed to parse LLM profile JSON: {}. Response: {}",
            e,
            &cleaned[..cleaned.len().min(200)]
        )
    })?;

    Ok((parsed.summary, parsed.detail))
}

// =============================================================================
// Heuristic fallback
// =============================================================================

/// Build a heuristic profile when LLM is unavailable.
///
/// Returns `(summary, Option<detail>)` using the original stats-based format.
fn generate_heuristic_profile(
    entity_kind: &str,
    entity_id: &str,
    entity_label: &str,
    obs_count: usize,
    finding_count: usize,
    resolved_count: usize,
    topics_str: &str,
    last_active: &str,
    pattern_ids: &[String],
) -> (String, Option<String>) {
    let profile_summary = format!(
        "{} -- {} observations, {} findings ({} resolved). Key topics: {}. Last active: {}.",
        entity_label, obs_count, finding_count, resolved_count, topics_str, last_active
    );

    let profile_detail = if obs_count > 0 || finding_count > 0 {
        let mut detail = String::new();
        detail.push_str(&format!(
            "Entity: {} (kind={}, id={})\n",
            entity_label, entity_kind, entity_id
        ));
        detail.push_str(&format!("Observations: {} total\n", obs_count));
        if topics_str != "none identified" {
            detail.push_str(&format!("Top topics: {}\n", topics_str));
        }
        detail.push_str(&format!(
            "Findings: {} total, {} resolved, {} open\n",
            finding_count,
            resolved_count,
            finding_count - resolved_count
        ));
        if !pattern_ids.is_empty() {
            detail.push_str(&format!("Cross-run patterns: {}\n", pattern_ids.len()));
        }
        Some(detail)
    } else {
        None
    };

    (profile_summary, profile_detail)
}
