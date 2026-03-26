//! Prompt extraction for the meta-prompt optimizer.
//!
//! Collects actual prompts used in recent runs by querying `phase_token_usage`,
//! joining with `learning_outcomes` for outcome scores, and grouping by
//! (phase, agent_type) to identify which prompts have the worst outcomes.

use rusqlite::params;
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::database::CheckpointDb;

/// A single prompt sample from a completed run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptSample {
    pub task_run_id: String,
    pub phase: String,
    pub agent_type: String,
    pub prompt_text: String,
    pub outcome_score: f64,
    pub outcome_status: String,
    pub iteration: u32,
    pub cost_usd: f64,
}

/// Aggregated metrics for a group of prompts sharing the same (phase, agent_type).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptGroupMetrics {
    pub phase: String,
    pub agent_type: String,
    pub sample_count: usize,
    pub mean_score: f64,
    pub failure_rate: f64,
    pub mean_iterations: f64,
    pub mean_cost: f64,
    /// Representative prompt text (from the most recent run).
    pub latest_prompt_text: String,
}

/// Extract prompt samples from recent runs, joining phase_token_usage with learning_outcomes.
///
/// Returns samples ordered by outcome_score ascending (worst first).
pub fn extract_prompt_samples(
    db: &CheckpointDb,
    limit: usize,
) -> Result<Vec<PromptSample>, String> {
    let limit = limit as i64;

    db.with_conn(move |conn| {
        // Join phase_token_usage with learning_outcomes via task_run_id → task_runs → learning_outcomes
        // phase_token_usage has phase, iteration; learning_outcomes has composite_agentic_score, status
        let mut stmt = conn
            .prepare(
                r#"SELECT
                       ptu.task_run_id,
                       ptu.phase,
                       COALESCE(tr.workflow_name, ptu.phase) AS agent_type,
                       COALESCE(lo.composite_agentic_score, 0.0) AS outcome_score,
                       COALESCE(lo.status, tr.status) AS outcome_status,
                       COALESCE(ptu.iteration, 0) AS iteration,
                       COALESCE(ptu.cost_cents, 0) / 100.0 AS cost_usd
                   FROM phase_token_usage ptu
                   INNER JOIN task_runs tr ON tr.id = ptu.task_run_id
                   LEFT JOIN learning_outcomes lo ON lo.task_id = tr.id
                   WHERE tr.status IN ('complete', 'failed')
                     AND COALESCE(tr.is_meta_optimizer, 0) = 0
                     AND COALESCE(tr.is_fixer, 0) = 0
                     AND COALESCE(tr.is_reflection, 0) = 0
                     AND COALESCE(tr.is_follow_up, 0) = 0
                     AND ptu.phase IN ('generation', 'verification', 'agentic')
                   ORDER BY outcome_score ASC
                   LIMIT ?1"#,
            )
            .map_err(|e| format!("Failed to prepare prompt extraction query: {}", e))?;

        let rows = stmt
            .query_map(params![limit], |row| {
                Ok(PromptSample {
                    task_run_id: row.get(0)?,
                    phase: row.get(1)?,
                    agent_type: row.get(2)?,
                    prompt_text: String::new(), // Populated separately if needed
                    outcome_score: row.get(3)?,
                    outcome_status: row.get(4)?,
                    iteration: row.get::<_, i64>(5)? as u32,
                    cost_usd: row.get(6)?,
                })
            })
            .map_err(|e| format!("Failed to execute prompt extraction query: {}", e))?
            .filter_map(|r| r.ok())
            .collect::<Vec<_>>();

        debug!("Extracted {} prompt samples", rows.len());
        Ok(rows)
    })
}

/// Group prompt samples by (phase, agent_type) and compute per-group metrics.
///
/// Enriches each group's `latest_prompt_text` from the active prompt variant
/// in the registry (if one exists), since `phase_token_usage` doesn't store
/// the full prompt content.
///
/// Returns groups sorted by mean_score ascending (worst-performing first).
pub fn compute_group_metrics(samples: &[PromptSample]) -> Vec<PromptGroupMetrics> {
    compute_group_metrics_inner(samples, None)
}

/// Like `compute_group_metrics` but enriches prompt text from the prompt registry.
pub fn compute_group_metrics_with_db(
    samples: &[PromptSample],
    db: &CheckpointDb,
) -> Vec<PromptGroupMetrics> {
    compute_group_metrics_inner(samples, Some(db))
}

fn compute_group_metrics_inner(
    samples: &[PromptSample],
    db: Option<&CheckpointDb>,
) -> Vec<PromptGroupMetrics> {
    use std::collections::HashMap;

    let mut groups: HashMap<(String, String), Vec<&PromptSample>> = HashMap::new();
    for sample in samples {
        groups
            .entry((sample.phase.clone(), sample.agent_type.clone()))
            .or_default()
            .push(sample);
    }

    let mut metrics: Vec<PromptGroupMetrics> = groups
        .into_iter()
        .map(|((phase, agent_type), group_samples)| {
            let count = group_samples.len();
            let sum_score: f64 = group_samples.iter().map(|s| s.outcome_score).sum();
            let failures = group_samples
                .iter()
                .filter(|s| {
                    s.outcome_status.to_lowercase() == "failed"
                        || s.outcome_status.to_lowercase() == "failure"
                })
                .count();
            let sum_iterations: f64 = group_samples.iter().map(|s| s.iteration as f64).sum();
            let sum_cost: f64 = group_samples.iter().map(|s| s.cost_usd).sum();

            // Try to get the active prompt variant text from the registry
            let latest_prompt = db
                .and_then(|db| {
                    super::prompt_registry::get_active_prompt(db, &agent_type)
                        .ok()
                        .flatten()
                        .map(|v| v.prompt_content)
                })
                .unwrap_or_default();

            PromptGroupMetrics {
                phase,
                agent_type,
                sample_count: count,
                mean_score: if count > 0 {
                    sum_score / count as f64
                } else {
                    0.0
                },
                failure_rate: if count > 0 {
                    failures as f64 / count as f64
                } else {
                    0.0
                },
                mean_iterations: if count > 0 {
                    sum_iterations / count as f64
                } else {
                    0.0
                },
                mean_cost: if count > 0 {
                    sum_cost / count as f64
                } else {
                    0.0
                },
                latest_prompt_text: latest_prompt,
            }
        })
        .collect();

    metrics.sort_by(|a, b| a.mean_score.partial_cmp(&b.mean_score).unwrap_or(std::cmp::Ordering::Equal));
    metrics
}

/// Get the worst-performing prompt group that meets the minimum sample threshold.
///
/// Returns None if no group has enough samples or if all groups exceed the
/// failure rate threshold (meaning they're doing well).
pub fn select_optimization_target(
    db: &CheckpointDb,
    min_samples: usize,
    min_failure_rate: f64,
) -> Result<Option<PromptGroupMetrics>, String> {
    let samples = extract_prompt_samples(db, 500)?;
    let groups = compute_group_metrics_with_db(&samples, db);

    let target = groups
        .into_iter()
        .find(|g| g.sample_count >= min_samples && g.failure_rate >= min_failure_rate);

    Ok(target)
}

/// Collect failure and success samples for a specific prompt group.
/// Returns (failures, successes) tuples.
pub fn collect_evidence_samples(
    db: &CheckpointDb,
    phase: &str,
    agent_type: &str,
    max_failures: usize,
    max_successes: usize,
) -> Result<(Vec<PromptSample>, Vec<PromptSample>), String> {
    let phase_owned = phase.to_string();
    let agent_type_owned = agent_type.to_string();
    let max_failures = max_failures as i64;
    let max_successes = max_successes as i64;

    db.with_conn(move |conn| {
        // Build optional agent_type filter
        let agent_filter = if agent_type_owned.is_empty() {
            String::new()
        } else {
            " AND COALESCE(tr.workflow_name, ptu.phase) = ?3".to_string()
        };

        // Failures (score < 0.5)
        let fail_sql = format!(
            r#"SELECT
                   ptu.task_run_id,
                   ptu.phase,
                   COALESCE(tr.workflow_name, ptu.phase),
                   COALESCE(lo.composite_agentic_score, 0.0),
                   COALESCE(lo.status, tr.status),
                   COALESCE(ptu.iteration, 0),
                   COALESCE(ptu.cost_cents, 0) / 100.0
               FROM phase_token_usage ptu
               INNER JOIN task_runs tr ON tr.id = ptu.task_run_id
               LEFT JOIN learning_outcomes lo ON lo.task_id = tr.id
               WHERE ptu.phase = ?1
                 AND tr.status IN ('complete', 'failed')
                 AND COALESCE(tr.is_meta_optimizer, 0) = 0
                 AND COALESCE(tr.is_fixer, 0) = 0
                 AND COALESCE(lo.composite_agentic_score, 0.0) < 0.5
                 {}
               ORDER BY lo.created_at DESC
               LIMIT ?2"#,
            agent_filter
        );

        let mut fail_stmt = conn
            .prepare(&fail_sql)
            .map_err(|e| format!("Failed to prepare failure query: {}", e))?;

        let fail_params: Vec<Box<dyn rusqlite::types::ToSql>> = if agent_type_owned.is_empty() {
            vec![Box::new(phase_owned.clone()), Box::new(max_failures)]
        } else {
            vec![
                Box::new(phase_owned.clone()),
                Box::new(max_failures),
                Box::new(agent_type_owned.clone()),
            ]
        };

        let failures: Vec<PromptSample> = fail_stmt
            .query_map(rusqlite::params_from_iter(fail_params.iter()), |row| {
                Ok(PromptSample {
                    task_run_id: row.get(0)?,
                    phase: row.get(1)?,
                    agent_type: row.get(2)?,
                    prompt_text: String::new(),
                    outcome_score: row.get(3)?,
                    outcome_status: row.get(4)?,
                    iteration: row.get::<_, i64>(5)? as u32,
                    cost_usd: row.get(6)?,
                })
            })
            .map_err(|e| format!("Failed to query failures: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        // Successes (score > 0.8)
        let success_sql = format!(
            r#"SELECT
                   ptu.task_run_id,
                   ptu.phase,
                   COALESCE(tr.workflow_name, ptu.phase),
                   COALESCE(lo.composite_agentic_score, 0.0),
                   COALESCE(lo.status, tr.status),
                   COALESCE(ptu.iteration, 0),
                   COALESCE(ptu.cost_cents, 0) / 100.0
               FROM phase_token_usage ptu
               INNER JOIN task_runs tr ON tr.id = ptu.task_run_id
               LEFT JOIN learning_outcomes lo ON lo.task_id = tr.id
               WHERE ptu.phase = ?1
                 AND tr.status IN ('complete', 'failed')
                 AND COALESCE(tr.is_meta_optimizer, 0) = 0
                 AND COALESCE(tr.is_fixer, 0) = 0
                 AND COALESCE(lo.composite_agentic_score, 0.0) > 0.8
                 {}
               ORDER BY lo.created_at DESC
               LIMIT ?2"#,
            agent_filter
        );

        let mut success_stmt = conn
            .prepare(&success_sql)
            .map_err(|e| format!("Failed to prepare success query: {}", e))?;

        let success_params: Vec<Box<dyn rusqlite::types::ToSql>> = if agent_type_owned.is_empty() {
            vec![Box::new(phase_owned), Box::new(max_successes)]
        } else {
            vec![
                Box::new(phase_owned),
                Box::new(max_successes),
                Box::new(agent_type_owned),
            ]
        };

        let successes: Vec<PromptSample> = success_stmt
            .query_map(rusqlite::params_from_iter(success_params.iter()), |row| {
                Ok(PromptSample {
                    task_run_id: row.get(0)?,
                    phase: row.get(1)?,
                    agent_type: row.get(2)?,
                    prompt_text: String::new(),
                    outcome_score: row.get(3)?,
                    outcome_status: row.get(4)?,
                    iteration: row.get::<_, i64>(5)? as u32,
                    cost_usd: row.get(6)?,
                })
            })
            .map_err(|e| format!("Failed to query successes: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok((failures, successes))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_group_metrics_empty() {
        let metrics = compute_group_metrics(&[]);
        assert!(metrics.is_empty());
    }

    #[test]
    fn test_compute_group_metrics_single_group() {
        let samples = vec![
            PromptSample {
                task_run_id: "run-1".to_string(),
                phase: "generation".to_string(),
                agent_type: "implementer".to_string(),
                prompt_text: String::new(),
                outcome_score: 0.3,
                outcome_status: "failed".to_string(),
                iteration: 2,
                cost_usd: 0.05,
            },
            PromptSample {
                task_run_id: "run-2".to_string(),
                phase: "generation".to_string(),
                agent_type: "implementer".to_string(),
                prompt_text: String::new(),
                outcome_score: 0.9,
                outcome_status: "complete".to_string(),
                iteration: 1,
                cost_usd: 0.03,
            },
        ];

        let metrics = compute_group_metrics(&samples);
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].phase, "generation");
        assert_eq!(metrics[0].agent_type, "implementer");
        assert_eq!(metrics[0].sample_count, 2);
        assert!((metrics[0].mean_score - 0.6).abs() < 0.01);
        assert!((metrics[0].failure_rate - 0.5).abs() < 0.01);
        assert!((metrics[0].mean_iterations - 1.5).abs() < 0.01);
    }

    #[test]
    fn test_compute_group_metrics_sorted_by_score() {
        let samples = vec![
            PromptSample {
                task_run_id: "run-1".to_string(),
                phase: "generation".to_string(),
                agent_type: "good_agent".to_string(),
                prompt_text: String::new(),
                outcome_score: 0.9,
                outcome_status: "complete".to_string(),
                iteration: 1,
                cost_usd: 0.01,
            },
            PromptSample {
                task_run_id: "run-2".to_string(),
                phase: "verification".to_string(),
                agent_type: "bad_agent".to_string(),
                prompt_text: String::new(),
                outcome_score: 0.1,
                outcome_status: "failed".to_string(),
                iteration: 5,
                cost_usd: 0.10,
            },
        ];

        let metrics = compute_group_metrics(&samples);
        assert_eq!(metrics.len(), 2);
        // Worst first
        assert_eq!(metrics[0].agent_type, "bad_agent");
        assert_eq!(metrics[1].agent_type, "good_agent");
    }

    #[test]
    fn test_compute_group_metrics_multiple_groups() {
        let samples = vec![
            PromptSample {
                task_run_id: "r1".to_string(),
                phase: "generation".to_string(),
                agent_type: "a".to_string(),
                prompt_text: String::new(),
                outcome_score: 0.5,
                outcome_status: "complete".to_string(),
                iteration: 1,
                cost_usd: 0.01,
            },
            PromptSample {
                task_run_id: "r2".to_string(),
                phase: "generation".to_string(),
                agent_type: "a".to_string(),
                prompt_text: String::new(),
                outcome_score: 0.7,
                outcome_status: "complete".to_string(),
                iteration: 2,
                cost_usd: 0.02,
            },
            PromptSample {
                task_run_id: "r3".to_string(),
                phase: "verification".to_string(),
                agent_type: "b".to_string(),
                prompt_text: String::new(),
                outcome_score: 0.2,
                outcome_status: "failed".to_string(),
                iteration: 3,
                cost_usd: 0.05,
            },
        ];

        let metrics = compute_group_metrics(&samples);
        assert_eq!(metrics.len(), 2);
        // Group b (score 0.2) should come before group a (score 0.6)
        assert_eq!(metrics[0].agent_type, "b");
        assert_eq!(metrics[1].agent_type, "a");
    }
}
