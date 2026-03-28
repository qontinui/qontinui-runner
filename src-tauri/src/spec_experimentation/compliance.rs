//! Spec compliance extraction and scoring.
//!
//! Reads existing `result_json` from `workflow_verification_phase_results`,
//! extracts per-assertion pass/fail data from `snapshot_assert` steps,
//! computes weighted compliance scores, and persists to `spec_compliance_results`.

use rusqlite::params;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::database::CheckpointDb;

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
pub fn extract_compliance(
    db: &CheckpointDb,
    task_run_id: &str,
) -> Result<SpecComplianceResult, String> {
    let trid = task_run_id.to_string();

    // 1. Get the latest iteration's result_json
        // PG: complex dynamic SQL
    let (iteration, result_json, spec_id): (i64, String, Option<String>) = db.with_conn({
        let trid = trid.clone();
        move |conn| {
            // Get result_json from the latest iteration
            let (iteration, result_json): (i64, String) = conn
                .query_row(
                    r#"SELECT iteration, result_json
                       FROM workflow_verification_phase_results
                       WHERE task_run_id = ?1
                       ORDER BY iteration DESC
                       LIMIT 1"#,
                    params![trid],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(|e| format!("No verification results for task_run_id {}: {}", trid, e))?;

            // Try to get the spec_id from the workflow's tags or category
            let spec_id: Option<String> = conn
                .query_row(
                    r#"SELECT uw.id
                       FROM task_runs tr
                       JOIN unified_workflows uw ON tr.workflow_id = uw.id
                       WHERE tr.id = ?1 AND uw.category = 'spec-generated'"#,
                    params![trid],
                    |row| row.get(0),
                )
                .ok();

            Ok((iteration, result_json, spec_id))
        }
    })?;

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

    // PG: complex dynamic SQL
    db.with_conn({
        let id = id.clone();
        let trid = trid.clone();
        let spec_id = spec_id.clone();
        let now = now.clone();
        move |conn| {
            conn.execute(
                r#"INSERT OR REPLACE INTO spec_compliance_results (
                    id, task_run_id, spec_id, iteration,
                    overall_score, raw_pass_rate,
                    critical_passed, critical_total,
                    warning_passed, warning_total,
                    info_passed, info_total,
                    assertions_passed, assertions_total,
                    group_scores_json, assertion_details_json,
                    created_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)"#,
                params![
                    id,
                    trid,
                    spec_id,
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
                    group_scores_json,
                    assertion_details_json,
                    now,
                ],
            )
            .map_err(|e| format!("Failed to insert spec_compliance_results: {}", e))?;
            Ok(())
        }
    })?;

    info!(
        "Extracted spec compliance for {}: score={:.3}, {}/{} assertions passed",
        trid, overall_score, assertions_passed, assertions_total
    );

    Ok(result)
}

/// Get compliance result for a specific task run (used by autoresearch enrichment).
pub fn get_compliance_for_run(
    db: &CheckpointDb,
    task_run_id: &str,
) -> Result<Option<SpecComplianceResult>, String> {
    let trid = task_run_id.to_string();
    // PG: complex dynamic SQL
    db.with_conn(move |conn| {
        let row = conn.query_row(
            r#"SELECT id, task_run_id, spec_id, iteration,
                          overall_score, raw_pass_rate,
                          critical_passed, critical_total,
                          warning_passed, warning_total,
                          info_passed, info_total,
                          assertions_passed, assertions_total,
                          group_scores_json, assertion_details_json,
                          created_at
                   FROM spec_compliance_results
                   WHERE task_run_id = ?1
                   ORDER BY created_at DESC LIMIT 1"#,
            params![trid],
            |row| {
                Ok(SpecComplianceResult {
                    id: row.get(0)?,
                    task_run_id: row.get(1)?,
                    spec_id: row.get(2)?,
                    iteration: row.get(3)?,
                    overall_score: row.get(4)?,
                    raw_pass_rate: row.get(5)?,
                    critical_passed: row.get(6)?,
                    critical_total: row.get(7)?,
                    warning_passed: row.get(8)?,
                    warning_total: row.get(9)?,
                    info_passed: row.get(10)?,
                    info_total: row.get(11)?,
                    assertions_passed: row.get(12)?,
                    assertions_total: row.get(13)?,
                    group_scores: serde_json::from_str(&row.get::<_, String>(14)?)
                        .unwrap_or_default(),
                    assertion_details: serde_json::from_str(&row.get::<_, String>(15)?)
                        .unwrap_or_default(),
                    created_at: row.get(16)?,
                })
            },
        );

        match row {
            Ok(r) => Ok(Some(r)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to query spec_compliance_results: {}", e)),
        }
    })
}

/// Get compliance history for a specific spec or all specs.
pub fn get_compliance_history(
    db: &CheckpointDb,
    spec_id: Option<&str>,
    limit: Option<i64>,
) -> Result<Vec<SpecComplianceResult>, String> {
    let spec_id = spec_id.map(|s| s.to_string());
    let limit = limit.unwrap_or(50);

    // PG: complex dynamic SQL
    db.with_conn(move |conn| {
        let (sql, params_vec): (String, Vec<Box<dyn rusqlite::types::ToSql>>) =
            if let Some(ref sid) = spec_id {
                (
                    r#"SELECT id, task_run_id, spec_id, iteration,
                          overall_score, raw_pass_rate,
                          critical_passed, critical_total,
                          warning_passed, warning_total,
                          info_passed, info_total,
                          assertions_passed, assertions_total,
                          group_scores_json, assertion_details_json,
                          created_at
                   FROM spec_compliance_results
                   WHERE spec_id = ?1
                   ORDER BY created_at DESC
                   LIMIT ?2"#
                        .to_string(),
                    vec![
                        Box::new(sid.clone()) as Box<dyn rusqlite::types::ToSql>,
                        Box::new(limit),
                    ],
                )
            } else {
                (
                    r#"SELECT id, task_run_id, spec_id, iteration,
                          overall_score, raw_pass_rate,
                          critical_passed, critical_total,
                          warning_passed, warning_total,
                          info_passed, info_total,
                          assertions_passed, assertions_total,
                          group_scores_json, assertion_details_json,
                          created_at
                   FROM spec_compliance_results
                   ORDER BY created_at DESC
                   LIMIT ?1"#
                        .to_string(),
                    vec![Box::new(limit) as Box<dyn rusqlite::types::ToSql>],
                )
            };

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare compliance history query: {}", e))?;

        let params_refs: Vec<&dyn rusqlite::types::ToSql> =
            params_vec.iter().map(|p| p.as_ref()).collect();

        let rows = stmt
            .query_map(params_refs.as_slice(), |row| {
                Ok(SpecComplianceResult {
                    id: row.get(0)?,
                    task_run_id: row.get(1)?,
                    spec_id: row.get(2)?,
                    iteration: row.get(3)?,
                    overall_score: row.get(4)?,
                    raw_pass_rate: row.get(5)?,
                    critical_passed: row.get(6)?,
                    critical_total: row.get(7)?,
                    warning_passed: row.get(8)?,
                    warning_total: row.get(9)?,
                    info_passed: row.get(10)?,
                    info_total: row.get(11)?,
                    assertions_passed: row.get(12)?,
                    assertions_total: row.get(13)?,
                    group_scores: serde_json::from_str(&row.get::<_, String>(14)?)
                        .unwrap_or_default(),
                    assertion_details: serde_json::from_str(&row.get::<_, String>(15)?)
                        .unwrap_or_default(),
                    created_at: row.get(16)?,
                })
            })
            .map_err(|e| format!("Failed to query compliance history: {}", e))?;

        let results: Vec<SpecComplianceResult> = rows.filter_map(|r| r.ok()).collect();
        Ok(results)
    })
}

/// Get a summary of compliance across all specs: latest score, trend, run count.
pub fn get_compliance_summary(db: &CheckpointDb) -> Result<Vec<SpecComplianceSummary>, String> {
    // PG: complex dynamic SQL
    db.with_conn(move |conn| {
        // Get distinct spec_ids with their latest scores and run counts
        let mut stmt = conn
            .prepare(
                r#"SELECT spec_id,
                          (SELECT overall_score FROM spec_compliance_results scr2
                           WHERE scr2.spec_id IS scr.spec_id
                           ORDER BY created_at DESC LIMIT 1) as latest_score,
                          COUNT(*) as run_count
                   FROM spec_compliance_results scr
                   GROUP BY spec_id
                   ORDER BY latest_score ASC"#,
            )
            .map_err(|e| format!("Failed to prepare compliance summary: {}", e))?;

        let rows = stmt
            .query_map([], |row| {
                let spec_id: Option<String> = row.get(0)?;
                let latest_score: f64 = row.get(1)?;
                let run_count: i64 = row.get(2)?;
                Ok((spec_id, latest_score, run_count))
            })
            .map_err(|e| format!("Failed to query compliance summary: {}", e))?;

        let mut summaries = Vec::new();
        for row in rows {
            let (spec_id, latest_score, run_count) = row.map_err(|e| e.to_string())?;

            // Compute trend from last 5 scores
            let trend = if run_count >= 3 {
                compute_trend(conn, spec_id.as_deref())
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
    })
}

/// Get average spec compliance score over a period (used by snapshots).
pub fn get_avg_compliance_since(db: &CheckpointDb, since: &str) -> Result<Option<f64>, String> {
    let since = since.to_string();
    // PG: complex dynamic SQL
    db.with_conn(move |conn| {
        let result: Result<f64, _> = conn.query_row(
            "SELECT AVG(overall_score) FROM spec_compliance_results WHERE created_at > ?1",
            params![since],
            |row| row.get(0),
        );
        match result {
            Ok(avg) => Ok(Some(avg)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(_) => Ok(None),
        }
    })
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

fn compute_trend(conn: &rusqlite::Connection, spec_id: Option<&str>) -> String {
    let scores: Vec<f64> = if let Some(sid) = spec_id {
        conn.prepare(
            "SELECT overall_score FROM spec_compliance_results WHERE spec_id = ?1 ORDER BY created_at DESC LIMIT 5",
        )
        .and_then(|mut stmt| {
            stmt.query_map(params![sid], |row| row.get(0))
                .map(|rows| rows.filter_map(|r| r.ok()).collect())
        })
        .unwrap_or_default()
    } else {
        conn.prepare(
            "SELECT overall_score FROM spec_compliance_results WHERE spec_id IS NULL ORDER BY created_at DESC LIMIT 5",
        )
        .and_then(|mut stmt| {
            stmt.query_map([], |row| row.get(0))
                .map(|rows| rows.filter_map(|r| r.ok()).collect())
        })
        .unwrap_or_default()
    };

    if scores.len() < 2 {
        return "stable".to_string();
    }

    // scores[0] is most recent, scores[last] is oldest
    let newest = scores[0];
    let oldest = scores[scores.len() - 1];
    let delta = newest - oldest;

    if delta > 0.03 {
        "improving".to_string()
    } else if delta < -0.03 {
        "declining".to_string()
    } else {
        "stable".to_string()
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
pub fn detect_broken_assertions(
    db: &CheckpointDb,
    spec_id: &str,
) -> Result<Vec<BrokenAssertion>, String> {
    let spec_id = spec_id.to_string();

    // PG: complex dynamic SQL
    db.with_conn(move |conn| {
        let mut stmt = conn
            .prepare(
                r#"SELECT assertion_details_json, created_at
                   FROM spec_compliance_results
                   WHERE spec_id = ?1
                   ORDER BY created_at DESC
                   LIMIT 20"#,
            )
            .map_err(|e| format!("Failed to prepare broken assertion query: {}", e))?;

        let runs: Vec<(String, String)> = stmt
            .query_map(params![spec_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| format!("Failed to query compliance runs: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        if runs.len() < 2 {
            return Ok(Vec::new());
        }

        let newest_details: Vec<AssertionDetail> =
            serde_json::from_str(&runs[0].0).unwrap_or_default();
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
                // Count consecutive failures from newest run backward.
                // We know runs[0] fails and runs[1] passes, so consecutive
                // starts at 1 and we check runs *before* runs[0] (i.e., check
                // if the assertion was also failing just before the pass).
                // Since runs[1] passed, the consecutive failure streak from
                // the most recent run is exactly 1. However, to handle cases
                // where runs[1] is the *only* pass sandwiched between failures,
                // we just report consecutive = 1 and note when it last passed.
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
    })
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
pub fn get_specs_needing_attention(db: &CheckpointDb) -> Result<Vec<SpecAttentionItem>, String> {
    // PG: complex dynamic SQL
    let spec_ids: Vec<(String, f64)> = db.with_conn(|conn| {
        let mut stmt = conn
            .prepare(
                r#"SELECT DISTINCT spec_id,
                          (SELECT overall_score FROM spec_compliance_results scr2
                           WHERE scr2.spec_id = scr.spec_id
                           ORDER BY created_at DESC LIMIT 1) as latest_score
                   FROM spec_compliance_results scr
                   WHERE spec_id IS NOT NULL"#,
            )
            .map_err(|e| format!("Failed to query spec_ids: {}", e))?;

        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
            })
            .map_err(|e| format!("Failed to iterate spec_ids: {}", e))?;

        Ok(rows.filter_map(|r| r.ok()).collect())
    })?;

    let mut attention_items: Vec<SpecAttentionItem> = Vec::new();

    for (spec_id, latest_score) in &spec_ids {
        let broken = detect_broken_assertions(db, spec_id)?;
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

    // PG: complex dynamic SQL
    let stale_specs: Vec<(String, i64)> = db.with_conn(|conn| {
        let mut stmt = conn
            .prepare(
                r#"SELECT spec_id, detail_json
                   FROM spec_accuracy_results
                   WHERE analysis_type = 'freshness'
                   ORDER BY created_at DESC"#,
            )
            .map_err(|e| format!("Failed to query freshness results: {}", e))?;

        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| format!("Failed to iterate freshness results: {}", e))?;

        let mut seen = std::collections::HashSet::new();
        let mut stale = Vec::new();
        for row in rows.flatten() {
            let (sid, detail_json) = row;
            if !seen.insert(sid.clone()) {
                continue;
            }
            if let Ok(detail) = serde_json::from_str::<serde_json::Value>(&detail_json) {
                let is_stale = detail
                    .get("is_stale")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let staleness_days = detail
                    .get("staleness_days")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                if is_stale && staleness_days > 0 {
                    stale.push((sid, staleness_days));
                }
            }
        }
        Ok(stale)
    })?;

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

    // PG: complex dynamic SQL
    let never_run: Vec<String> = db.with_conn(|conn| {
        let mut stmt = conn
            .prepare(
                r#"SELECT DISTINCT sv.spec_id
                   FROM spec_versions sv
                   WHERE NOT EXISTS (
                       SELECT 1 FROM spec_compliance_results scr
                       WHERE scr.spec_id = sv.spec_id
                   )"#,
            )
            .map_err(|e| format!("Failed to query never-run specs: {}", e))?;

        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| format!("Failed to iterate never-run specs: {}", e))?;

        Ok(rows.filter_map(|r| r.ok()).collect())
    })?;

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
pub fn auto_extract_spec_compliance(db: &CheckpointDb, task_run_id: &str) {
    let trid = task_run_id.to_string();

    // Check if this is a spec-generated workflow
    let is_spec = db
        // PG: complex dynamic SQL
        .with_conn({
            let trid = trid.clone();
            move |conn| {
                let category: Option<String> = conn
                    .query_row(
                        r#"SELECT uw.category
                           FROM task_runs tr
                           JOIN unified_workflows uw ON tr.workflow_id = uw.id
                           WHERE tr.id = ?1"#,
                        params![trid],
                        |row| row.get(0),
                    )
                    .ok();
                Ok(category == Some("spec-generated".to_string()))
            }
        })
        .unwrap_or(false);

    if !is_spec {
        return;
    }

    match extract_compliance(db, &trid) {
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
