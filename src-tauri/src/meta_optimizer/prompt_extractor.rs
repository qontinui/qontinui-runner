//! Prompt extraction for the meta-prompt optimizer.
//!
//! Collects actual prompts used in recent runs by querying `phase_token_usage`,
//! joining with `learning_outcomes` for outcome scores, and grouping by
//! (phase, agent_type) to identify which prompts have the worst outcomes.

use std::sync::Arc;
use serde::{Deserialize, Serialize};
use tokio::runtime::Handle;
use tracing::debug;

use crate::database::CheckpointDb;
use crate::database::pg::PgDb;

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
    // Get pg_db from the app state via a thread-local or pass it through
    // For backward compat, delegate to extract_prompt_samples_pg with a fallback
    // This function is called from select_optimization_target which only has db
    // We'll use the PG path via the _with_pg wrapper
    extract_prompt_samples_sqlite(db, limit)
}

/// Extract prompt samples using PG (primary path).
pub fn extract_prompt_samples_pg(
    pg_db: &Arc<PgDb>,
    limit: usize,
) -> Result<Vec<PromptSample>, String> {
    let limit = limit as i64;

    let rows = tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.extract_prompt_samples(limit))
    })?;

    let samples: Vec<PromptSample> = rows.into_iter().map(|(task_run_id, phase, agent_type, outcome_score, outcome_status, iteration, cost_usd)| {
        PromptSample {
            task_run_id,
            phase,
            agent_type,
            prompt_text: String::new(),
            outcome_score,
            outcome_status,
            iteration: iteration as u32,
            cost_usd,
        }
    }).collect();

    debug!("Extracted {} prompt samples (PG)", samples.len());
    Ok(samples)
}

/// SQLite fallback for extract_prompt_samples (kept for backward compat).
fn extract_prompt_samples_sqlite(
    db: &CheckpointDb,
    limit: usize,
) -> Result<Vec<PromptSample>, String> {
    let limit = limit as i64;

    db.with_conn(move |conn| {
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
            .query_map(rusqlite::params![limit], |row| {
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

            // Try to get the active prompt variant text from the registry,
            // falling back to the hardcoded default for the agent type.
            let latest_prompt = db
                .and_then(|db| {
                    super::prompt_registry::get_active_prompt_sqlite(db, &agent_type)
                        .ok()
                        .flatten()
                        .map(|v| v.prompt_content)
                })
                .unwrap_or_else(|| get_default_agent_prompt(&agent_type));

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

/// Get the default/hardcoded prompt for a known agent type.
///
/// When no prompt variant is active in the registry, this returns the
/// built-in prompt that the pipeline uses by default. This ensures the
/// meta-prompt optimizer can see and critique the actual prompt being used,
/// even before any variants have been created.
pub fn get_default_agent_prompt(agent_type: &str) -> String {
    match agent_type {
        "locator" => {
            // The locator prompt is a template with placeholders — return the
            // structural template so the optimizer can see the instructions.
            r#"You are a code locator agent. Given acceptance criteria and a project directory structure, identify which files are most relevant to each criterion.

## Acceptance Criteria
{criteria_text}

## Project Directory Structure (L0 — directories only)
{file_tree}

## Instructions
1. Review the directory structure above and identify 2-4 directories most relevant to each criterion.
2. Use tool use to list files within those directories.
3. For each criterion, identify 1-5 files that are most likely to need changes or inspection.
4. Rate your confidence from 0.0 to 1.0 in each mapping.

Output JSON array with: criterion_id, spec_assertion_id, description, target_files, related_files, confidence."#.to_string()
        }
        "verifier" | "verification" => {
            crate::autoresearch::agentic_verification::AgenticVerificationPrompts::verifier_system_prompt(
                "{goal}",
                None,
            )
        }
        "implementer" => {
            r#"Multi-Agent Pipeline: Implement the acceptance criteria for the specified subtree.

You are an implementer agent in a multi-agent pipeline. Your job is to make code changes that satisfy the acceptance criteria provided by the spec analyst, targeting the files identified by the locator agent.

## Instructions
1. Read the target files identified by the locator
2. Understand the acceptance criteria requirements
3. Make the minimum code changes needed to satisfy each criterion
4. Verify your changes compile and don't break existing functionality
5. Report what you changed and why

Focus on correctness over cleverness. Make the smallest change that satisfies the criteria."#.to_string()
        }
        "agentic_fixer" => {
            r#"You are a code fixer agent. Your job is to fix verification failures identified in the previous iteration.

Review the failure output carefully, identify the root cause, and make targeted fixes. Do not make unrelated changes."#.to_string()
        }
        _ => String::new(),
    }
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
    // Kept as SQLite fallback for callers without pg_db
    collect_evidence_samples_sqlite(db, phase, agent_type, max_failures, max_successes)
}

/// Collect evidence samples using PG (primary path).
pub fn collect_evidence_samples_pg(
    pg_db: &Arc<PgDb>,
    phase: &str,
    agent_type: &str,
    max_failures: usize,
    max_successes: usize,
) -> Result<(Vec<PromptSample>, Vec<PromptSample>), String> {
    let (fail_tuples, success_tuples) = tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.collect_evidence_samples(
            phase, agent_type, max_failures as i64, max_successes as i64,
        ))
    })?;

    let to_sample = |t: (String, String, String, f64, String, i64, f64)| -> PromptSample {
        PromptSample {
            task_run_id: t.0, phase: t.1, agent_type: t.2, prompt_text: String::new(),
            outcome_score: t.3, outcome_status: t.4, iteration: t.5 as u32, cost_usd: t.6,
        }
    };

    Ok((
        fail_tuples.into_iter().map(to_sample).collect(),
        success_tuples.into_iter().map(to_sample).collect(),
    ))
}

/// SQLite fallback for collect_evidence_samples.
fn collect_evidence_samples_sqlite(
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
        let agent_filter = if agent_type_owned.is_empty() {
            String::new()
        } else {
            " AND COALESCE(tr.workflow_name, ptu.phase) = ?3".to_string()
        };

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

        let mut fail_stmt = conn.prepare(&fail_sql)
            .map_err(|e| format!("Failed to prepare failure query: {}", e))?;

        let fail_params: Vec<Box<dyn rusqlite::types::ToSql>> = if agent_type_owned.is_empty() {
            vec![Box::new(phase_owned.clone()), Box::new(max_failures)]
        } else {
            vec![Box::new(phase_owned.clone()), Box::new(max_failures), Box::new(agent_type_owned.clone())]
        };

        let failures: Vec<PromptSample> = fail_stmt
            .query_map(rusqlite::params_from_iter(fail_params.iter()), |row| {
                Ok(PromptSample {
                    task_run_id: row.get(0)?, phase: row.get(1)?, agent_type: row.get(2)?,
                    prompt_text: String::new(), outcome_score: row.get(3)?,
                    outcome_status: row.get(4)?, iteration: row.get::<_, i64>(5)? as u32, cost_usd: row.get(6)?,
                })
            })
            .map_err(|e| format!("Failed to query failures: {}", e))?
            .filter_map(|r| r.ok()).collect();

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

        let mut success_stmt = conn.prepare(&success_sql)
            .map_err(|e| format!("Failed to prepare success query: {}", e))?;

        let success_params: Vec<Box<dyn rusqlite::types::ToSql>> = if agent_type_owned.is_empty() {
            vec![Box::new(phase_owned), Box::new(max_successes)]
        } else {
            vec![Box::new(phase_owned), Box::new(max_successes), Box::new(agent_type_owned)]
        };

        let successes: Vec<PromptSample> = success_stmt
            .query_map(rusqlite::params_from_iter(success_params.iter()), |row| {
                Ok(PromptSample {
                    task_run_id: row.get(0)?, phase: row.get(1)?, agent_type: row.get(2)?,
                    prompt_text: String::new(), outcome_score: row.get(3)?,
                    outcome_status: row.get(4)?, iteration: row.get::<_, i64>(5)? as u32, cost_usd: row.get(6)?,
                })
            })
            .map_err(|e| format!("Failed to query successes: {}", e))?
            .filter_map(|r| r.ok()).collect();

        Ok((failures, successes))
    })
}

// ─��� PG dual-write wrappers ────────────────────────────��─────────────────

/// Extract prompt samples with PG-primary.
pub fn extract_prompt_samples_with_pg(
    _db: &CheckpointDb,
    pg_db: &Arc<PgDb>,
    limit: usize,
) -> Result<Vec<PromptSample>, String> {
    extract_prompt_samples_pg(pg_db, limit)
}

/// Compute group metrics with PG fallback (currently delegates to SQLite).
pub fn compute_group_metrics_with_db_pg(
    samples: &[PromptSample],
    db: &CheckpointDb,
    _pg_db: &Arc<PgDb>,
) -> Vec<PromptGroupMetrics> {
    compute_group_metrics_with_db(samples, db)
}

/// Collect evidence samples with PG-primary.
pub fn collect_evidence_samples_with_pg(
    _db: &CheckpointDb,
    pg_db: &Arc<PgDb>,
    phase: &str,
    agent_type: &str,
    max_failures: usize,
    max_successes: usize,
) -> Result<(Vec<PromptSample>, Vec<PromptSample>), String> {
    collect_evidence_samples_pg(pg_db, phase, agent_type, max_failures, max_successes)
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
