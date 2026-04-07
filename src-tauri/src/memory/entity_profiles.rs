//! Entity profile generation logic.
//!
//! Gathers evidence from observations, findings, and patterns to build
//! heuristic entity profiles. Profiles are refreshed periodically for
//! stale entities.

use tracing::{info, warn};

use crate::database::pg::PgDb;
use crate::database::types::CreateEntityProfileInput;

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

    // 3. Search findings related to this entity
    let conn = pg
        .pool()
        .get()
        .await
        .map_err(|e| format!("PG pool error: {}", e))?;

    let finding_rows = conn
        .query(
            "SELECT id, status FROM task_run_findings
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

    let resolved_count = finding_rows
        .iter()
        .filter(|r| {
            let status: String = r.get(1);
            status == "resolved" || status == "fixed"
        })
        .count();

    // 4. Search cross-run patterns
    let pattern_rows = conn
        .query(
            "SELECT id FROM cross_run_patterns
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

    // 5. Determine last active date
    let last_active = observations
        .iter()
        .map(|o| o.updated_at.as_str())
        .max()
        .unwrap_or("unknown")
        .to_string();

    // 6. Build summary
    let topics_str = if top_topics.is_empty() {
        "none identified".to_string()
    } else {
        top_topics.join(", ")
    };

    let profile_summary = format!(
        "{} -- {} observations, {} findings ({} resolved). Key topics: {}. Last active: {}.",
        entity_label, obs_count, finding_count, resolved_count, topics_str, last_active
    );

    // 7. Build detail (more verbose)
    let profile_detail = if obs_count > 0 || finding_count > 0 {
        let mut detail = String::new();
        detail.push_str(&format!("Entity: {} (kind={}, id={})\n", entity_label, entity_kind, entity_id));
        detail.push_str(&format!("Observations: {} total\n", obs_count));
        if !top_topics.is_empty() {
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
