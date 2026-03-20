//! CRUD operations for the meta_optimizer_recommendations table.
//!
//! All optimizer outputs go here with status `pending`. Human reviews from UI.

use rusqlite::params;
use serde::Deserialize;
use tracing::{info, warn};

use super::types::{MetaOptimizerRun, Recommendation};
use crate::database::CheckpointDb;

// ── JSON payloads expected inside `recommended_value` ────────────────

#[derive(Debug, Deserialize)]
struct PromptRewritePayload {
    agent_type: String,
    variant_name: String,
    prompt_content: String,
}

#[derive(Debug, Deserialize)]
struct ConfigChangePayload {
    key: String,
    value: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct RulePayload {
    agent: String,
    section: String,
    title: String,
    content: String,
    #[serde(default)]
    condition: Option<String>,
    #[serde(default)]
    rule_number: Option<i32>,
    /// For rule_update: the ID of the existing rule to update.
    #[serde(default)]
    rule_id: Option<String>,
    /// For disabling a rule.
    #[serde(default)]
    status: Option<String>,
}

/// Create a new recommendation.
pub fn create_recommendation(
    db: &CheckpointDb,
    optimizer_type: &str,
    recommendation_type: &str,
    target_agent: Option<&str>,
    title: &str,
    description: &str,
    current_value: Option<&str>,
    recommended_value: Option<&str>,
    evidence: Option<&str>,
    confidence: f64,
    optimizer_run_id: Option<&str>,
) -> Result<Recommendation, String> {
    let id = format!("mor-{}", uuid::Uuid::new_v4());
    let now = chrono::Utc::now().to_rfc3339();

    let rec = Recommendation {
        id: id.clone(),
        optimizer_type: optimizer_type.to_string(),
        recommendation_type: recommendation_type.to_string(),
        target_agent: target_agent.map(|s| s.to_string()),
        title: title.to_string(),
        description: description.to_string(),
        current_value: current_value.map(|s| s.to_string()),
        recommended_value: recommended_value.map(|s| s.to_string()),
        evidence: evidence.map(|s| s.to_string()),
        confidence,
        status: "pending".to_string(),
        applied_at: None,
        outcome_after_apply: None,
        optimizer_run_id: optimizer_run_id.map(|s| s.to_string()),
        created_at: now.clone(),
    };

    let rec_clone = rec.clone();
    db.with_conn(move |conn| {
        conn.execute(
            r#"INSERT INTO meta_optimizer_recommendations
               (id, optimizer_type, recommendation_type, target_agent, title, description,
                current_value, recommended_value, evidence, confidence, status,
                optimizer_run_id, created_at)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'pending', ?11, ?12)"#,
            params![
                rec_clone.id,
                rec_clone.optimizer_type,
                rec_clone.recommendation_type,
                rec_clone.target_agent,
                rec_clone.title,
                rec_clone.description,
                rec_clone.current_value,
                rec_clone.recommended_value,
                rec_clone.evidence,
                rec_clone.confidence,
                rec_clone.optimizer_run_id,
                rec_clone.created_at,
            ],
        )
        .map_err(|e| format!("Failed to create recommendation: {}", e))?;

        info!(
            "Created recommendation {} ({})",
            rec_clone.id, rec_clone.title
        );
        Ok(())
    })?;

    Ok(rec)
}

/// List recommendations with optional filters.
pub fn list_recommendations(
    db: &CheckpointDb,
    optimizer_type: Option<&str>,
    status: Option<&str>,
) -> Result<Vec<Recommendation>, String> {
    let optimizer_type = optimizer_type.map(|s| s.to_string());
    let status = status.map(|s| s.to_string());

    db.with_conn(move |conn| {
        let mut sql = String::from(
            r#"SELECT id, optimizer_type, recommendation_type, target_agent, title, description,
                      current_value, recommended_value, evidence, confidence, status,
                      applied_at, outcome_after_apply, optimizer_run_id, created_at
               FROM meta_optimizer_recommendations WHERE 1=1"#,
        );
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let mut param_idx = 1;

        if let Some(ref ot) = optimizer_type {
            sql.push_str(&format!(" AND optimizer_type = ?{}", param_idx));
            param_values.push(Box::new(ot.clone()));
            param_idx += 1;
        }
        if let Some(ref st) = status {
            sql.push_str(&format!(" AND status = ?{}", param_idx));
            param_values.push(Box::new(st.clone()));
        }
        sql.push_str(" ORDER BY created_at DESC");

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let rows = stmt
            .query_map(rusqlite::params_from_iter(param_values.iter()), |row| {
                Ok(Recommendation {
                    id: row.get(0)?,
                    optimizer_type: row.get(1)?,
                    recommendation_type: row.get(2)?,
                    target_agent: row.get(3)?,
                    title: row.get(4)?,
                    description: row.get(5)?,
                    current_value: row.get(6)?,
                    recommended_value: row.get(7)?,
                    evidence: row.get(8)?,
                    confidence: row.get(9)?,
                    status: row.get(10)?,
                    applied_at: row.get(11)?,
                    outcome_after_apply: row.get(12)?,
                    optimizer_run_id: row.get(13)?,
                    created_at: row.get(14)?,
                })
            })
            .map_err(|e| format!("Failed to query recommendations: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(rows)
    })
}

/// Apply a recommendation (updates status to 'applied').
/// This is the simple status-only flip — prefer `apply_recommendation_with_side_effects`.
pub fn apply_recommendation(db: &CheckpointDb, recommendation_id: &str) -> Result<(), String> {
    let id = recommendation_id.to_string();
    let now = chrono::Utc::now().to_rfc3339();

    db.with_conn(move |conn| {
        let affected = conn
            .execute(
                "UPDATE meta_optimizer_recommendations SET status = 'applied', applied_at = ?1 WHERE id = ?2 AND status = 'pending'",
                params![now, id],
            )
            .map_err(|e| format!("Failed to apply recommendation: {}", e))?;

        if affected == 0 {
            return Err(format!("Recommendation {} not found or not pending", id));
        }

        info!("Applied recommendation {}", id);
        Ok(())
    })
}

/// Fetch a single recommendation by ID.
fn get_recommendation(
    db: &CheckpointDb,
    recommendation_id: &str,
) -> Result<Recommendation, String> {
    let id = recommendation_id.to_string();
    db.with_conn(move |conn| {
        conn.query_row(
            r#"SELECT id, optimizer_type, recommendation_type, target_agent, title, description,
                      current_value, recommended_value, evidence, confidence, status,
                      applied_at, outcome_after_apply, optimizer_run_id, created_at
               FROM meta_optimizer_recommendations WHERE id = ?1"#,
            params![id],
            |row| {
                Ok(Recommendation {
                    id: row.get(0)?,
                    optimizer_type: row.get(1)?,
                    recommendation_type: row.get(2)?,
                    target_agent: row.get(3)?,
                    title: row.get(4)?,
                    description: row.get(5)?,
                    current_value: row.get(6)?,
                    recommended_value: row.get(7)?,
                    evidence: row.get(8)?,
                    confidence: row.get(9)?,
                    status: row.get(10)?,
                    applied_at: row.get(11)?,
                    outcome_after_apply: row.get(12)?,
                    optimizer_run_id: row.get(13)?,
                    created_at: row.get(14)?,
                })
            },
        )
        .map_err(|e| format!("Recommendation not found: {}", e))
    })
}

/// Apply a recommendation **and** perform the appropriate side-effect based on
/// `recommendation_type`. If the side-effect fails the status is NOT updated.
pub fn apply_recommendation_with_side_effects(
    db: &CheckpointDb,
    recommendation_id: &str,
) -> Result<(), String> {
    let rec = get_recommendation(db, recommendation_id)?;

    if rec.status != "pending" {
        return Err(format!(
            "Recommendation {} is not pending (status: {})",
            rec.id, rec.status
        ));
    }

    let recommended_value = rec
        .recommended_value
        .as_deref()
        .ok_or_else(|| format!("Recommendation {} has no recommended_value", rec.id))?;

    match rec.recommendation_type.as_str() {
        "prompt_rewrite" => apply_prompt_rewrite(db, &rec.id, recommended_value)?,
        "config_change" => apply_config_change(db, recommended_value)?,
        "rule_create" => apply_rule_create(db, &rec.id, recommended_value)?,
        "rule_update" => apply_rule_update(db, recommended_value)?,
        other => {
            warn!(
                "Unknown recommendation_type '{}' for {}; applying status-only",
                other, rec.id
            );
        }
    }

    // Side-effect succeeded — now flip the status
    apply_recommendation(db, recommendation_id)?;

    // Capture a snapshot to measure impact of this recommendation
    if let Err(e) = super::snapshots::capture_post_apply(
        db,
        recommendation_id,
        super::types::WorkflowCategory::Main,
    ) {
        warn!("Failed to capture post-apply snapshot: {}", e);
    }

    // Evaluate outcome immediately (will likely be "insufficient_data" initially)
    if let Err(e) = super::snapshots::evaluate_recommendation_outcome(db, recommendation_id) {
        warn!("Failed to evaluate recommendation outcome: {}", e);
    }

    Ok(())
}

// ── Side-effect helpers ──────────────────────────────────────────────

fn apply_prompt_rewrite(
    db: &CheckpointDb,
    recommendation_id: &str,
    recommended_value: &str,
) -> Result<(), String> {
    let payload: PromptRewritePayload = serde_json::from_str(recommended_value)
        .map_err(|e| format!("Invalid prompt_rewrite payload: {}", e))?;

    let variant = super::prompt_registry::create_variant(
        db,
        &payload.agent_type,
        &payload.variant_name,
        &payload.prompt_content,
        Some(recommendation_id),
    )?;

    super::prompt_registry::activate_variant(db, &variant.id)?;

    info!(
        "Applied prompt_rewrite recommendation {}: created and activated variant {}",
        recommendation_id, variant.id
    );
    Ok(())
}

fn apply_config_change(db: &CheckpointDb, recommended_value: &str) -> Result<(), String> {
    let payload: ConfigChangePayload = serde_json::from_str(recommended_value)
        .map_err(|e| format!("Invalid config_change payload: {}", e))?;

    db.set_setting(&payload.key, &payload.value)?;

    info!(
        "Applied config_change: set '{}' to {}",
        payload.key, payload.value
    );
    Ok(())
}

fn apply_rule_create(
    db: &CheckpointDb,
    recommendation_id: &str,
    recommended_value: &str,
) -> Result<(), String> {
    let payload: RulePayload = serde_json::from_str(recommended_value)
        .map_err(|e| format!("Invalid rule_create payload: {}", e))?;

    let rec_id = recommendation_id.to_string();
    db.with_conn(move |conn| {
        let rule_number = payload.rule_number.unwrap_or_else(|| {
            crate::workflow_generation::rules::next_rule_number(
                conn,
                &payload.agent,
                &payload.section,
            )
        });

        let input = crate::workflow_generation::rules::InsertRuleInput {
            agent: payload.agent.clone(),
            section: payload.section.clone(),
            rule_number,
            title: payload.title.clone(),
            content: payload.content.clone(),
            condition: payload.condition.clone(),
            provenance: "meta_optimizer".to_string(),
            source_fix_id: Some(rec_id.clone()),
        };

        let rule = crate::workflow_generation::rules::insert_rule(conn, &input)?;
        info!(
            "Applied rule_create recommendation {}: created rule {}",
            rec_id, rule.id
        );
        Ok(())
    })
}

fn apply_rule_update(db: &CheckpointDb, recommended_value: &str) -> Result<(), String> {
    let payload: RulePayload = serde_json::from_str(recommended_value)
        .map_err(|e| format!("Invalid rule_update payload: {}", e))?;

    let rule_id = payload
        .rule_id
        .ok_or_else(|| "rule_update payload missing 'rule_id'".to_string())?;

    db.with_conn(move |conn| {
        let input = crate::workflow_generation::rules::UpdateRuleInput {
            title: Some(payload.title),
            content: Some(payload.content),
            condition: payload.condition,
            status: payload.status,
            rule_number: payload.rule_number,
        };

        let rule = crate::workflow_generation::rules::update_rule(conn, &rule_id, &input)?;
        info!(
            "Applied rule_update: updated rule {} ({})",
            rule.id, rule.title
        );
        Ok(())
    })
}

/// Reject a recommendation.
pub fn reject_recommendation(db: &CheckpointDb, recommendation_id: &str) -> Result<(), String> {
    let id = recommendation_id.to_string();

    db.with_conn(move |conn| {
        let affected = conn
            .execute(
                "UPDATE meta_optimizer_recommendations SET status = 'rejected' WHERE id = ?1 AND status = 'pending'",
                params![id],
            )
            .map_err(|e| format!("Failed to reject recommendation: {}", e))?;

        if affected == 0 {
            return Err(format!("Recommendation {} not found or not pending", id));
        }

        info!("Rejected recommendation {}", id);
        Ok(())
    })
}

/// Roll back an applied recommendation, undoing side-effects where possible.
pub fn rollback_recommendation(db: &CheckpointDb, recommendation_id: &str) -> Result<(), String> {
    let rec = get_recommendation(db, recommendation_id)?;

    if rec.status != "applied" {
        return Err(format!(
            "Recommendation {} is not applied (status: {})",
            rec.id, rec.status
        ));
    }

    // Attempt to undo the side-effect. Failures here are logged but do not
    // prevent the status from being rolled back — the user explicitly requested
    // the rollback.
    if let Some(ref recommended_value) = rec.recommended_value {
        match rec.recommendation_type.as_str() {
            "rule_create" | "rule_update" => {
                if let Err(e) = rollback_rule(db, &rec.id, recommended_value) {
                    warn!("Failed to rollback rule side-effect for {}: {}", rec.id, e);
                }
            }
            "config_change" => {
                if let Some(ref current_value) = rec.current_value {
                    if let Err(e) = rollback_config_change(db, current_value) {
                        warn!(
                            "Failed to rollback config side-effect for {}: {}",
                            rec.id, e
                        );
                    }
                }
            }
            "prompt_rewrite" => {
                // Prompt variants are not automatically deactivated on rollback.
                // The user can manually switch prompts via the prompt registry UI.
                info!(
                    "Prompt rollback for {} — variant left in registry, user can deactivate manually",
                    rec.id
                );
            }
            _ => {}
        }
    }

    // Flip the status to rolled_back
    let id = recommendation_id.to_string();
    db.with_conn(move |conn| {
        let affected = conn
            .execute(
                "UPDATE meta_optimizer_recommendations SET status = 'rolled_back' WHERE id = ?1 AND status = 'applied'",
                params![id],
            )
            .map_err(|e| format!("Failed to rollback recommendation: {}", e))?;

        if affected == 0 {
            return Err(format!("Recommendation {} not found or not applied", id));
        }

        info!("Rolled back recommendation {}", id);
        Ok(())
    })
}

/// Undo a rule side-effect by disabling the rule that was created/updated.
fn rollback_rule(
    db: &CheckpointDb,
    recommendation_id: &str,
    recommended_value: &str,
) -> Result<(), String> {
    // For rule_create, find the rule by source_fix_id matching the recommendation ID.
    // For rule_update, parse the rule_id from the payload.
    let rule_id_from_payload: Option<String> =
        serde_json::from_str::<RulePayload>(recommended_value)
            .ok()
            .and_then(|p| p.rule_id);

    let rec_id = recommendation_id.to_string();

    db.with_conn(move |conn| {
        let target_rule_id = if let Some(id) = rule_id_from_payload {
            id
        } else {
            // Find rule created by this recommendation (source_fix_id = recommendation_id)
            conn.query_row(
                "SELECT id FROM generation_rules WHERE source_fix_id = ?1 LIMIT 1",
                params![rec_id],
                |row| row.get::<_, String>(0),
            )
            .map_err(|e| {
                format!(
                    "Could not find rule created by recommendation {}: {}",
                    rec_id, e
                )
            })?
        };

        let input = crate::workflow_generation::rules::UpdateRuleInput {
            title: None,
            content: None,
            condition: None,
            status: Some("disabled".to_string()),
            rule_number: None,
        };

        crate::workflow_generation::rules::update_rule(conn, &target_rule_id, &input)?;
        info!("Rollback: disabled rule {}", target_rule_id);
        Ok(())
    })
}

/// Undo a config change by restoring the `current_value`.
fn rollback_config_change(db: &CheckpointDb, current_value: &str) -> Result<(), String> {
    let payload: ConfigChangePayload = serde_json::from_str(current_value)
        .map_err(|e| format!("Invalid current_value payload for config rollback: {}", e))?;

    db.set_setting(&payload.key, &payload.value)?;
    info!(
        "Rollback: restored config '{}' to {}",
        payload.key, payload.value
    );
    Ok(())
}

// ── Meta-Optimizer Runs ────────────────────────────────────────────────

/// Create a new optimizer run record.
pub fn create_optimizer_run(
    db: &CheckpointDb,
    optimizer_type: &str,
    trigger_type: &str,
    task_run_id: Option<&str>,
) -> Result<String, String> {
    let id = format!("morun-{}", uuid::Uuid::new_v4());
    let now = chrono::Utc::now().to_rfc3339();
    let id_clone = id.clone();
    let optimizer_type = optimizer_type.to_string();
    let trigger_type = trigger_type.to_string();
    let task_run_id = task_run_id.map(|s| s.to_string());

    db.with_conn(move |conn| {
        conn.execute(
            r#"INSERT INTO meta_optimizer_runs
               (id, optimizer_type, trigger_type, runs_analyzed, recommendations_produced,
                task_run_id, status, created_at)
               VALUES (?1, ?2, ?3, 0, 0, ?4, 'running', ?5)"#,
            params![id_clone, optimizer_type, trigger_type, task_run_id, now],
        )
        .map_err(|e| format!("Failed to create optimizer run: {}", e))?;
        Ok(())
    })?;

    Ok(id)
}

/// Complete an optimizer run, recording how many runs were analyzed and recommendations produced.
pub fn complete_optimizer_run(
    db: &CheckpointDb,
    run_id: &str,
    runs_analyzed: i64,
    recommendations_produced: i64,
) -> Result<(), String> {
    let run_id = run_id.to_string();
    let now = chrono::Utc::now().to_rfc3339();

    db.with_conn(move |conn| {
        conn.execute(
            r#"UPDATE meta_optimizer_runs
               SET status = 'complete', runs_analyzed = ?1, recommendations_produced = ?2, completed_at = ?3
               WHERE id = ?4"#,
            params![runs_analyzed, recommendations_produced, now, run_id],
        )
        .map_err(|e| format!("Failed to complete optimizer run: {}", e))?;
        Ok(())
    })
}

/// List optimizer runs.
pub fn list_optimizer_runs(db: &CheckpointDb) -> Result<Vec<MetaOptimizerRun>, String> {
    db.with_conn(|conn| {
        let mut stmt = conn
            .prepare(
                r#"SELECT id, optimizer_type, trigger_type, runs_analyzed, recommendations_produced,
                          task_run_id, status, created_at, completed_at
                   FROM meta_optimizer_runs
                   ORDER BY created_at DESC
                   LIMIT 100"#,
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let rows = stmt
            .query_map([], |row| {
                Ok(MetaOptimizerRun {
                    id: row.get(0)?,
                    optimizer_type: row.get(1)?,
                    trigger_type: row.get(2)?,
                    runs_analyzed: row.get(3)?,
                    recommendations_produced: row.get(4)?,
                    task_run_id: row.get(5)?,
                    status: row.get(6)?,
                    created_at: row.get(7)?,
                    completed_at: row.get(8)?,
                })
            })
            .map_err(|e| format!("Failed to query runs: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(rows)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::CheckpointDb;

    fn setup_test_db() -> CheckpointDb {
        CheckpointDb::new_in_memory().unwrap()
    }

    fn make_recommendation(db: &CheckpointDb, title: &str, optimizer_type: &str) -> Recommendation {
        create_recommendation(
            db,
            optimizer_type,
            "prompt_change",
            Some("planner"),
            title,
            "Test description",
            Some(r#"{"old": true}"#),
            Some(r#"{"new": true}"#),
            Some(r#"{"score": 0.9}"#),
            0.85,
            None,
        )
        .unwrap()
    }

    #[test]
    fn test_create_and_list_recommendations() {
        let db = setup_test_db();

        let rec1 = make_recommendation(&db, "Improve planner prompt", "pipeline_prompt");
        let rec2 = make_recommendation(&db, "Adjust architecture", "architecture");

        assert!(rec1.id.starts_with("mor-"));
        assert_eq!(rec1.status, "pending");
        assert_eq!(rec1.confidence, 0.85);

        let all = list_recommendations(&db, None, None).unwrap();
        assert_eq!(all.len(), 2);

        // Verify fields round-trip correctly
        let found = all.iter().find(|r| r.id == rec1.id).unwrap();
        assert_eq!(found.title, "Improve planner prompt");
        assert_eq!(found.optimizer_type, "pipeline_prompt");
        assert_eq!(found.target_agent.as_deref(), Some("planner"));
        assert_eq!(found.current_value.as_deref(), Some(r#"{"old": true}"#));
        assert_eq!(found.recommended_value.as_deref(), Some(r#"{"new": true}"#));
        assert_eq!(found.evidence.as_deref(), Some(r#"{"score": 0.9}"#));

        // rec2 also present
        assert!(all.iter().any(|r| r.id == rec2.id));
    }

    #[test]
    fn test_apply_recommendation() {
        let db = setup_test_db();
        let rec = make_recommendation(&db, "Apply me", "pipeline_prompt");

        apply_recommendation(&db, &rec.id).unwrap();

        let all = list_recommendations(&db, None, Some("applied")).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, rec.id);
        assert_eq!(all[0].status, "applied");
        assert!(all[0].applied_at.is_some());
    }

    #[test]
    fn test_reject_recommendation() {
        let db = setup_test_db();
        let rec = make_recommendation(&db, "Reject me", "pipeline_prompt");

        reject_recommendation(&db, &rec.id).unwrap();

        let all = list_recommendations(&db, None, Some("rejected")).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, rec.id);
        assert_eq!(all[0].status, "rejected");
    }

    #[test]
    fn test_rollback_applied() {
        let db = setup_test_db();
        let rec = make_recommendation(&db, "Rollback me", "pipeline_prompt");

        apply_recommendation(&db, &rec.id).unwrap();
        rollback_recommendation(&db, &rec.id).unwrap();

        let all = list_recommendations(&db, None, Some("rolled_back")).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, rec.id);
        assert_eq!(all[0].status, "rolled_back");
    }

    #[test]
    fn test_cannot_apply_non_pending() {
        let db = setup_test_db();
        let rec = make_recommendation(&db, "Already rejected", "pipeline_prompt");

        reject_recommendation(&db, &rec.id).unwrap();

        let result = apply_recommendation(&db, &rec.id);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found or not pending"));
    }

    #[test]
    fn test_list_filters() {
        let db = setup_test_db();

        let rec1 = make_recommendation(&db, "Prompt rec", "pipeline_prompt");
        let _rec2 = make_recommendation(&db, "Arch rec", "architecture");
        let rec3 = make_recommendation(&db, "Another prompt rec", "pipeline_prompt");

        // Apply rec1 so we have mixed statuses
        apply_recommendation(&db, &rec1.id).unwrap();

        // Filter by optimizer_type
        let prompt_recs = list_recommendations(&db, Some("pipeline_prompt"), None).unwrap();
        assert_eq!(prompt_recs.len(), 2);

        let arch_recs = list_recommendations(&db, Some("architecture"), None).unwrap();
        assert_eq!(arch_recs.len(), 1);

        // Filter by status
        let pending = list_recommendations(&db, None, Some("pending")).unwrap();
        assert_eq!(pending.len(), 2); // rec2 and rec3

        let applied = list_recommendations(&db, None, Some("applied")).unwrap();
        assert_eq!(applied.len(), 1);

        // Filter by both
        let prompt_pending =
            list_recommendations(&db, Some("pipeline_prompt"), Some("pending")).unwrap();
        assert_eq!(prompt_pending.len(), 1);
        assert_eq!(prompt_pending[0].id, rec3.id);
    }

    #[test]
    fn test_optimizer_run_lifecycle() {
        let db = setup_test_db();

        let run_id = create_optimizer_run(&db, "pipeline_prompt", "threshold", None).unwrap();
        assert!(run_id.starts_with("morun-"));

        // List runs — should show as running
        let runs = list_optimizer_runs(&db).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, run_id);
        assert_eq!(runs[0].status, "running");
        assert_eq!(runs[0].optimizer_type, "pipeline_prompt");
        assert_eq!(runs[0].trigger_type, "threshold");
        assert_eq!(runs[0].runs_analyzed, 0);
        assert_eq!(runs[0].recommendations_produced, 0);
        assert!(runs[0].completed_at.is_none());

        // Complete the run
        complete_optimizer_run(&db, &run_id, 10, 3).unwrap();

        let runs = list_optimizer_runs(&db).unwrap();
        assert_eq!(runs[0].status, "complete");
        assert_eq!(runs[0].runs_analyzed, 10);
        assert_eq!(runs[0].recommendations_produced, 3);
        assert!(runs[0].completed_at.is_some());
    }
}
