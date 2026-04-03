//! Spec compliance extraction and scoring.
//!
//! Reads existing `result_json` from `workflow_verification_phase_results`,
//! extracts per-assertion pass/fail data from `snapshot_assert` steps,
//! computes weighted compliance scores, and persists to `spec_compliance_results`.

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::database::pg::PgDb;

// ── Severity weights ──────────────────────────────────────────────────

const SEVERITY_WEIGHTS: &[(&str, f64)] = &[("critical", 3.0), ("warning", 1.0), ("info", 0.5)];

fn weight_for_severity(severity: &str) -> f64 {
    SEVERITY_WEIGHTS
        .iter()
        .find(|(s, _)| *s == severity)
        .map(|(_, w)| *w)
        .unwrap_or(0.5)
}

// ── Types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpecComplianceResult {
    pub id: String,
    pub task_run_id: String,
    pub spec_id: Option<String>,
    pub iteration: i64,
    pub overall_score: f64,
    pub raw_pass_rate: f64,
    pub critical_passed: i64,
    pub critical_total: i64,
    pub warning_passed: i64,
    pub warning_total: i64,
    pub info_passed: i64,
    pub info_total: i64,
    pub assertions_passed: i64,
    pub assertions_total: i64,
    pub group_scores: Vec<GroupScore>,
    pub assertion_details: Vec<AssertionDetail>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupScore {
    pub group_name: String,
    pub score: f64,
    pub passed: i64,
    pub total: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssertionDetail {
    pub id: String,
    pub description: String,
    pub severity: String,
    pub assertion_type: String,
    pub passed: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpecComplianceSummary {
    pub spec_id: Option<String>,
    pub latest_score: f64,
    pub trend: String, // "improving", "declining", "stable"
    pub run_count: i64,
}

// ── Extraction ────────────────────────────────────────────────────────

/// Extract compliance metrics from existing result_json in workflow_verification_phase_results.
pub async fn extract_compliance(
    pg: &PgDb,
    task_run_id: &str,
) -> Result<SpecComplianceResult, String> {
    let trid = task_run_id.to_string();

    // 1. Get the latest iteration's result_json
    let (iteration, result_json, spec_id) = pg.get_latest_verification_result(&trid).await?;

    // 2. Parse the result_json
    let result: serde_json::Value = serde_json::from_str(&result_json)
        .map_err(|e| format!("Failed to parse result_json: {}", e))?;

    // 3. Extract assertion results from step_results
    let mut all_details: Vec<AssertionDetail> = Vec::new();
    let mut group_map: std::collections::HashMap<String, (i64, i64)> =
        std::collections::HashMap::new();

    if let Some(step_results) = result.get("step_results").and_then(|v| v.as_array()) {
        for step in step_results {
            let step_name = step
                .get("step_name")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();

            // Check output_data for snapshot_assert action
            let output_data = step.get("output_data");
            let action = output_data
                .and_then(|od| od.get("action"))
                .and_then(|a| a.as_str());

            if action == Some("snapshot_assert") {
                if let Some(results) = output_data
                    .and_then(|od| od.get("results"))
                    .and_then(|r| r.as_array())
                {
                    let group_entry = group_map.entry(step_name.clone()).or_insert((0, 0));
                    for assertion_result in results {
                        let detail = parse_assertion_detail(assertion_result);
                        if detail.passed {
                            group_entry.0 += 1;
                        }
                        group_entry.1 += 1;
                        all_details.push(detail);
                    }
                }
            }
        }
    }

    // 4. Count by severity
    let mut critical_passed: i64 = 0;
    let mut critical_total: i64 = 0;
    let mut warning_passed: i64 = 0;
    let mut warning_total: i64 = 0;
    let mut info_passed: i64 = 0;
    let mut info_total: i64 = 0;

    for d in &all_details {
        match d.severity.as_str() {
            "critical" => {
                critical_total += 1;
                if d.passed {
                    critical_passed += 1;
                }
            }
            "warning" => {
                warning_total += 1;
                if d.passed {
                    warning_passed += 1;
                }
            }
            _ => {
                info_total += 1;
                if d.passed {
                    info_passed += 1;
                }
            }
        }
    }

    let assertions_total = all_details.len() as i64;
    let assertions_passed = all_details.iter().filter(|d| d.passed).count() as i64;

    // 5. Compute weighted overall_score
    let overall_score = compute_weighted_score(&all_details);

    let raw_pass_rate = if assertions_total > 0 {
        assertions_passed as f64 / assertions_total as f64
    } else {
        1.0
    };

    // 6. Per-group scores
    let group_scores: Vec<GroupScore> = group_map
        .into_iter()
        .map(|(name, (passed, total))| GroupScore {
            score: if total > 0 {
                passed as f64 / total as f64
            } else {
                1.0
            },
            group_name: name,
            passed,
            total,
        })
        .collect();

    // 7. Persist
    let id = format!("scr-{}", uuid::Uuid::new_v4());
    let now = chrono::Utc::now().to_rfc3339();

    let result = SpecComplianceResult {
        id: id.clone(),
        task_run_id: trid.clone(),
        spec_id: spec_id.clone(),
        iteration,
        overall_score,
        raw_pass_rate,
        critical_passed,
        critical_total,
        warning_passed,
        warning_total,
        info_passed,
        info_total,
        assertions_passed,
        assertions_total,
        group_scores: group_scores.clone(),
        assertion_details: all_details.clone(),
        created_at: now.clone(),
    };

    let group_scores_json = serde_json::to_string(&group_scores).unwrap_or_else(|_| "[]".into());
    let assertion_details_json =
        serde_json::to_string(&all_details).unwrap_or_else(|_| "[]".into());

    pg.upsert_spec_compliance_result(
        &id,
        &trid,
        spec_id.as_deref(),
        iteration,
        overall_score,
        raw_pass_rate,
        critical_passed,
        critical_total,
        warning_passed,
        warning_total,
        info_passed,
        info_total,
        assertions_passed,
        assertions_total,
        &group_scores_json,
        &assertion_details_json,
        &now,
    )
    .await?;

    info!(
        "Extracted spec compliance for {}: score={:.3}, {}/{} assertions passed",
        trid, overall_score, assertions_passed, assertions_total
    );

    Ok(result)
}

/// Get compliance result for a specific task run (used by autoresearch enrichment).
pub async fn get_compliance_for_run(
    pg: &PgDb,
    task_run_id: &str,
) -> Result<Option<SpecComplianceResult>, String> {
    let row = pg.get_compliance_for_run(task_run_id).await?;
    Ok(row.map(|r| SpecComplianceResult {
        id: r.id,
        task_run_id: r.task_run_id,
        spec_id: r.spec_id,
        iteration: r.iteration,
        overall_score: r.overall_score,
        raw_pass_rate: r.raw_pass_rate,
        critical_passed: r.critical_passed,
        critical_total: r.critical_total,
        warning_passed: r.warning_passed,
        warning_total: r.warning_total,
        info_passed: r.info_passed,
        info_total: r.info_total,
        assertions_passed: r.assertions_passed,
        assertions_total: r.assertions_total,
        group_scores: serde_json::from_str(&r.group_scores_json).unwrap_or_default(),
        assertion_details: serde_json::from_str(&r.assertion_details_json).unwrap_or_default(),
        created_at: r.created_at,
    }))
}

/// Get compliance history for a specific spec or all specs.
pub async fn get_compliance_history(
    pg: &PgDb,
    spec_id: Option<&str>,
    limit: Option<i64>,
) -> Result<Vec<SpecComplianceResult>, String> {
    let limit = limit.unwrap_or(50);
    let rows = pg.get_compliance_history(spec_id, limit).await?;
    Ok(rows
        .into_iter()
        .map(|r| SpecComplianceResult {
            id: r.id,
            task_run_id: r.task_run_id,
            spec_id: r.spec_id,
            iteration: r.iteration,
            overall_score: r.overall_score,
            raw_pass_rate: r.raw_pass_rate,
            critical_passed: r.critical_passed,
            critical_total: r.critical_total,
            warning_passed: r.warning_passed,
            warning_total: r.warning_total,
            info_passed: r.info_passed,
            info_total: r.info_total,
            assertions_passed: r.assertions_passed,
            assertions_total: r.assertions_total,
            group_scores: serde_json::from_str(&r.group_scores_json).unwrap_or_default(),
            assertion_details: serde_json::from_str(&r.assertion_details_json).unwrap_or_default(),
            created_at: r.created_at,
        })
        .collect())
}

/// Get a summary of compliance across all specs: latest score, trend, run count.
pub async fn get_compliance_summary(pg: &PgDb) -> Result<Vec<SpecComplianceSummary>, String> {
    let rows = pg.get_compliance_summary().await?;

    let mut summaries = Vec::new();
    for (spec_id, latest_score, run_count) in rows {
        let trend = if run_count >= 3 {
            pg.get_compliance_trend(spec_id.as_deref()).await?
        } else {
            "stable".to_string()
        };

        summaries.push(SpecComplianceSummary {
            spec_id,
            latest_score,
            trend,
            run_count,
        });
    }

    Ok(summaries)
}

/// Get average spec compliance score over a period (used by snapshots).
pub async fn get_avg_compliance_since(pg: &PgDb, since: &str) -> Result<Option<f64>, String> {
    pg.get_avg_compliance_since(since).await
}

// ── Helpers ───────────────────────────────────────────────────────────

fn parse_assertion_detail(v: &serde_json::Value) -> AssertionDetail {
    AssertionDetail {
        id: v
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        description: v
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        severity: v
            .get("severity")
            .and_then(|v| v.as_str())
            .unwrap_or("info")
            .to_string(),
        assertion_type: v
            .get("assertionType")
            .and_then(|v| v.as_str())
            .unwrap_or("exists")
            .to_string(),
        passed: v.get("passed").and_then(|v| v.as_bool()).unwrap_or(false),
        detail: v
            .get("detail")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
    }
}

fn compute_weighted_score(details: &[AssertionDetail]) -> f64 {
    if details.is_empty() {
        return 1.0;
    }

    let mut weighted_passed = 0.0;
    let mut weighted_total = 0.0;

    for d in details {
        let w = weight_for_severity(&d.severity);
        weighted_total += w;
        if d.passed {
            weighted_passed += w;
        }
    }

    if weighted_total > 0.0 {
        weighted_passed / weighted_total
    } else {
        1.0
    }
}

// -- Broken assertion detection ------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrokenAssertion {
    pub assertion_id: String,
    pub description: String,
    pub severity: String,
    pub last_passed_at: Option<String>,
    pub first_failed_at: String,
    pub consecutive_failures: u32,
    pub detail: String,
}

/// Identify assertions that were passing in previous runs but are now failing.
/// These are likely regressions caused by code changes.
pub async fn detect_broken_assertions(
    pg: &PgDb,
    spec_id: &str,
) -> Result<Vec<BrokenAssertion>, String> {
    let runs = pg
        .get_compliance_runs_for_broken_detection(spec_id, 20)
        .await?;

    if runs.len() < 2 {
        return Ok(Vec::new());
    }

    let newest_details: Vec<AssertionDetail> = serde_json::from_str(&runs[0].0).unwrap_or_default();
    let previous_details: Vec<AssertionDetail> =
        serde_json::from_str(&runs[1].0).unwrap_or_default();

    let prev_map: std::collections::HashMap<String, bool> = previous_details
        .iter()
        .map(|d| (d.id.clone(), d.passed))
        .collect();

    let mut broken = Vec::new();

    for detail in &newest_details {
        if detail.id.is_empty() {
            continue;
        }
        let was_passing = prev_map.get(&detail.id).copied().unwrap_or(false);
        if was_passing && !detail.passed {
            let consecutive = 1u32;
            let first_failed_at = runs[0].1.clone();
            let last_passed_at = Some(runs[1].1.clone());

            broken.push(BrokenAssertion {
                assertion_id: detail.id.clone(),
                description: detail.description.clone(),
                severity: detail.severity.clone(),
                last_passed_at,
                first_failed_at,
                consecutive_failures: consecutive,
                detail: detail.detail.clone(),
            });
        }
    }

    Ok(broken)
}

// -- Spec attention detection -------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpecAttentionItem {
    pub spec_id: String,
    pub reason: String,
    pub broken_count: u32,
    pub staleness_days: i64,
    pub latest_score: Option<f64>,
    pub detail: String,
}

/// Get specs that need attention: broken assertions, stale, or never run.
pub async fn get_specs_needing_attention(pg: &PgDb) -> Result<Vec<SpecAttentionItem>, String> {
    let spec_ids = pg.get_spec_ids_with_latest_scores().await?;

    let mut attention_items: Vec<SpecAttentionItem> = Vec::new();

    for (spec_id, latest_score) in &spec_ids {
        let broken = detect_broken_assertions(pg, spec_id).await?;
        if !broken.is_empty() {
            let critical_count = broken.iter().filter(|b| b.severity == "critical").count();
            let detail = if critical_count > 0 {
                format!(
                    "{} assertion(s) regressed ({} critical): {}",
                    broken.len(),
                    critical_count,
                    broken
                        .iter()
                        .take(3)
                        .map(|b| b.description.clone())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            } else {
                format!(
                    "{} assertion(s) regressed: {}",
                    broken.len(),
                    broken
                        .iter()
                        .take(3)
                        .map(|b| b.description.clone())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };

            attention_items.push(SpecAttentionItem {
                spec_id: spec_id.clone(),
                reason: "broken_assertions".to_string(),
                broken_count: broken.len() as u32,
                staleness_days: 0,
                latest_score: Some(*latest_score),
                detail,
            });
        }
    }

    let stale_specs = pg.get_stale_specs_from_accuracy().await?;

    for (spec_id, staleness_days) in stale_specs {
        if let Some(item) = attention_items.iter_mut().find(|a| a.spec_id == spec_id) {
            item.staleness_days = staleness_days;
            item.detail = format!("{} (also stale by {}d)", item.detail, staleness_days);
            continue;
        }

        let latest_score = spec_ids
            .iter()
            .find(|(id, _)| id == &spec_id)
            .map(|(_, s)| *s);

        attention_items.push(SpecAttentionItem {
            spec_id,
            reason: "stale".to_string(),
            broken_count: 0,
            staleness_days,
            latest_score,
            detail: format!("Spec is {}d older than its component files", staleness_days),
        });
    }

    let never_run = pg.get_never_run_spec_ids().await?;

    for spec_id in never_run {
        attention_items.push(SpecAttentionItem {
            spec_id,
            reason: "never_run".to_string(),
            broken_count: 0,
            staleness_days: 0,
            latest_score: None,
            detail: "Spec has never been compliance-checked".to_string(),
        });
    }

    attention_items.sort_by(|a, b| {
        let order = |r: &str| match r {
            "broken_assertions" => 0,
            "stale" => 1,
            "never_run" => 2,
            _ => 3,
        };
        order(&a.reason)
            .cmp(&order(&b.reason))
            .then(b.broken_count.cmp(&a.broken_count))
    });

    Ok(attention_items)
}

/// Auto-extract compliance for a spec-generated workflow.
/// Called from the meta_optimizer trigger hook.
pub async fn auto_extract_spec_compliance(pg: &PgDb, task_run_id: &str) {
    let trid = task_run_id.to_string();

    // Check if this is a spec-generated workflow
    let is_spec = pg.is_spec_generated_workflow(&trid).await.unwrap_or(false);

    if !is_spec {
        return;
    }

    match extract_compliance(pg, &trid).await {
        Ok(result) => {
            info!(
                "Auto-extracted spec compliance for {}: {:.1}%",
                trid,
                result.overall_score * 100.0
            );
        }
        Err(e) => {
            warn!("Failed to auto-extract spec compliance for {}: {}", trid, e);
        }
    }
}
