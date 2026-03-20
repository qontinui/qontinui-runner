//! Temporal Trend Analysis
//!
//! Queries convergence_snapshots and component_health_snapshots to provide
//! time-series trend data for workflow convergence and component health.

use rusqlite::{params, Connection};
use serde::Serialize;
use uuid::Uuid;

// =============================================================================
// Types
// =============================================================================

#[derive(Debug, Clone, Serialize)]
pub struct TrendPoint {
    pub timestamp: String,
    pub value: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkflowTrends {
    pub workflow_name: String,
    pub convergence: Vec<TrendPoint>,
    pub fix_rate: Vec<TrendPoint>,
    pub velocity: Vec<TrendPoint>,
    pub total_fixes: Vec<TrendPoint>,
    pub snapshot_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComponentTrend {
    pub component_path: String,
    pub health_scores: Vec<TrendPoint>,
    pub fix_counts: Vec<TrendPoint>,
}

// =============================================================================
// Time Range Parsing
// =============================================================================

/// Parses a time range string like "7d", "24h", "30d" into an ISO datetime cutoff.
/// Returns None for "all" or invalid input (meaning no filtering).
fn parse_time_cutoff(time_range: Option<&str>) -> Option<String> {
    let range = time_range?;
    if range == "all" {
        return None;
    }

    let now = chrono::Utc::now();
    let duration = if range.ends_with('d') {
        let days: i64 = range.trim_end_matches('d').parse().ok()?;
        chrono::Duration::days(days)
    } else if range.ends_with('h') {
        let hours: i64 = range.trim_end_matches('h').parse().ok()?;
        chrono::Duration::hours(hours)
    } else {
        return None;
    };

    let cutoff = now - duration;
    Some(cutoff.format("%Y-%m-%dT%H:%M:%SZ").to_string())
}

// =============================================================================
// Workflow Trends (from convergence_snapshots)
// =============================================================================

/// Query workflow-level trend data from convergence_snapshots.
pub fn get_workflow_trends(
    conn: &Connection,
    workflow_name: &str,
    time_range: Option<&str>,
) -> Result<WorkflowTrends, String> {
    let cutoff = parse_time_cutoff(time_range);

    let (sql, query_params): (&str, Vec<Box<dyn rusqlite::types::ToSql>>) =
        if let Some(ref cutoff_str) = cutoff {
            (
                r#"SELECT snapshot_at, convergence_score, effective_fix_rate,
                      change_velocity, total_fixes, effective_fixes
               FROM convergence_snapshots
               WHERE workflow_name = ?1 AND snapshot_at >= ?2
               ORDER BY snapshot_at ASC"#,
                vec![
                    Box::new(workflow_name.to_string()) as Box<dyn rusqlite::types::ToSql>,
                    Box::new(cutoff_str.clone()),
                ],
            )
        } else {
            (
                r#"SELECT snapshot_at, convergence_score, effective_fix_rate,
                      change_velocity, total_fixes, effective_fixes
               FROM convergence_snapshots
               WHERE workflow_name = ?1
               ORDER BY snapshot_at ASC"#,
                vec![Box::new(workflow_name.to_string()) as Box<dyn rusqlite::types::ToSql>],
            )
        };

    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| format!("Failed to prepare workflow trends query: {}", e))?;

    let params_refs: Vec<&dyn rusqlite::types::ToSql> =
        query_params.iter().map(|p| p.as_ref()).collect();

    let rows = stmt
        .query_map(params_refs.as_slice(), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, f64>(1)?,
                row.get::<_, f64>(2)?,
                row.get::<_, f64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })
        .map_err(|e| format!("Failed to query workflow trends: {}", e))?;

    let mut convergence = Vec::new();
    let mut fix_rate = Vec::new();
    let mut velocity = Vec::new();
    let mut total_fixes = Vec::new();
    let mut snapshot_count = 0usize;

    for row in rows {
        let (timestamp, conv_score, eff_rate, vel, total, effective) =
            row.map_err(|e| format!("Failed to read trend row: {}", e))?;

        convergence.push(TrendPoint {
            timestamp: timestamp.clone(),
            value: conv_score,
            count: None,
        });
        fix_rate.push(TrendPoint {
            timestamp: timestamp.clone(),
            value: eff_rate,
            count: Some(effective as u32),
        });
        velocity.push(TrendPoint {
            timestamp: timestamp.clone(),
            value: vel,
            count: None,
        });
        total_fixes.push(TrendPoint {
            timestamp: timestamp.clone(),
            value: total as f64,
            count: None,
        });
        snapshot_count += 1;
    }

    Ok(WorkflowTrends {
        workflow_name: workflow_name.to_string(),
        convergence,
        fix_rate,
        velocity,
        total_fixes,
        snapshot_count,
    })
}

// =============================================================================
// Component Trends (from component_health_snapshots)
// =============================================================================

/// Query per-component trend data from component_health_snapshots.
pub fn get_component_trend(
    conn: &Connection,
    workflow_name: &str,
    component_path: &str,
    time_range: Option<&str>,
) -> Result<ComponentTrend, String> {
    let cutoff = parse_time_cutoff(time_range);

    let (sql, query_params): (&str, Vec<Box<dyn rusqlite::types::ToSql>>) =
        if let Some(ref cutoff_str) = cutoff {
            (
                r#"SELECT snapshot_at, health_score, fix_count, effective_fix_count
               FROM component_health_snapshots
               WHERE workflow_name = ?1 AND component_path = ?2 AND snapshot_at >= ?3
               ORDER BY snapshot_at ASC"#,
                vec![
                    Box::new(workflow_name.to_string()) as Box<dyn rusqlite::types::ToSql>,
                    Box::new(component_path.to_string()),
                    Box::new(cutoff_str.clone()),
                ],
            )
        } else {
            (
                r#"SELECT snapshot_at, health_score, fix_count, effective_fix_count
               FROM component_health_snapshots
               WHERE workflow_name = ?1 AND component_path = ?2
               ORDER BY snapshot_at ASC"#,
                vec![
                    Box::new(workflow_name.to_string()) as Box<dyn rusqlite::types::ToSql>,
                    Box::new(component_path.to_string()),
                ],
            )
        };

    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| format!("Failed to prepare component trend query: {}", e))?;

    let params_refs: Vec<&dyn rusqlite::types::ToSql> =
        query_params.iter().map(|p| p.as_ref()).collect();

    let rows = stmt
        .query_map(params_refs.as_slice(), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, f64>(1)?,
                row.get::<_, i32>(2)?,
                row.get::<_, i32>(3)?,
            ))
        })
        .map_err(|e| format!("Failed to query component trends: {}", e))?;

    let mut health_scores = Vec::new();
    let mut fix_counts = Vec::new();

    for row in rows {
        let (timestamp, health, fixes, effective) =
            row.map_err(|e| format!("Failed to read component trend row: {}", e))?;

        health_scores.push(TrendPoint {
            timestamp: timestamp.clone(),
            value: health,
            count: None,
        });
        fix_counts.push(TrendPoint {
            timestamp: timestamp.clone(),
            value: fixes as f64,
            count: Some(effective as u32),
        });
    }

    Ok(ComponentTrend {
        component_path: component_path.to_string(),
        health_scores,
        fix_counts,
    })
}

// =============================================================================
// Effectiveness Over Time (from reflection_fixes)
// =============================================================================

#[derive(Debug, Clone, Serialize)]
pub struct EffectivenessBucket {
    pub bucket: String,
    pub total: u32,
    pub effective: u32,
    pub ineffective: u32,
    pub regression: u32,
    pub effectiveness_rate: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct EffectivenessOverTime {
    pub workflow_name: String,
    pub bucket_type: String,
    pub buckets: Vec<EffectivenessBucket>,
}

/// Query effectiveness rate bucketed by time from `reflection_fixes`.
///
/// `bucket_type`: "week" (default) or "month".
/// `time_range`: "7d", "30d", "all", etc.
pub fn get_effectiveness_over_time(
    conn: &Connection,
    workflow_name: &str,
    bucket_type: &str,
    time_range: Option<&str>,
) -> Result<EffectivenessOverTime, String> {
    let cutoff = parse_time_cutoff(time_range);

    let strftime_fmt = match bucket_type {
        "month" => "%Y-%m",
        _ => "%Y-W%W",
    };

    let base_sql = format!(
        r#"SELECT strftime('{fmt}', evaluated_at) AS bucket,
                  COUNT(*) AS total,
                  SUM(CASE WHEN effectiveness = 'effective' THEN 1 ELSE 0 END) AS effective,
                  SUM(CASE WHEN effectiveness = 'ineffective' THEN 1 ELSE 0 END) AS ineffective,
                  SUM(CASE WHEN effectiveness = 'regression' THEN 1 ELSE 0 END) AS regression
           FROM reflection_fixes
           WHERE source_task_run_id IN (
               SELECT id FROM task_runs WHERE workflow_name = ?1
           )
           AND evaluated_at IS NOT NULL
           {cutoff_clause}
           GROUP BY bucket
           ORDER BY bucket ASC"#,
        fmt = strftime_fmt,
        cutoff_clause = if cutoff.is_some() {
            "AND evaluated_at >= ?2"
        } else {
            ""
        }
    );

    let mut stmt = conn
        .prepare(&base_sql)
        .map_err(|e| format!("Failed to prepare effectiveness query: {}", e))?;

    let query_params: Vec<Box<dyn rusqlite::types::ToSql>> = if let Some(ref c) = cutoff {
        vec![Box::new(workflow_name.to_string()), Box::new(c.clone())]
    } else {
        vec![Box::new(workflow_name.to_string())]
    };
    let params_refs: Vec<&dyn rusqlite::types::ToSql> =
        query_params.iter().map(|p| p.as_ref()).collect();

    let rows = stmt
        .query_map(params_refs.as_slice(), |row| {
            let bucket: String = row.get(0)?;
            let total: u32 = row.get(1)?;
            let effective: u32 = row.get(2)?;
            let ineffective: u32 = row.get(3)?;
            let regression: u32 = row.get(4)?;
            Ok((bucket, total, effective, ineffective, regression))
        })
        .map_err(|e| format!("Failed to query effectiveness: {}", e))?;

    let mut buckets = Vec::new();
    for row in rows {
        let (bucket, total, effective, ineffective, regression) =
            row.map_err(|e| format!("Failed to read effectiveness row: {}", e))?;
        let rate = if total > 0 {
            effective as f64 / total as f64
        } else {
            0.0
        };
        buckets.push(EffectivenessBucket {
            bucket,
            total,
            effective,
            ineffective,
            regression,
            effectiveness_rate: rate,
        });
    }

    Ok(EffectivenessOverTime {
        workflow_name: workflow_name.to_string(),
        bucket_type: bucket_type.to_string(),
        buckets,
    })
}

// =============================================================================
// Snapshot Storage (called from architecture rebuild)
// =============================================================================

/// Store component health snapshots during architecture model rebuild.
/// Each tuple is (component_path, health_score, fix_count, effective_fix_count, change_velocity).
pub fn store_component_health_snapshots(
    conn: &Connection,
    workflow_name: &str,
    components: &[(String, f64, i32, i32, f64)],
) -> Result<usize, String> {
    let mut count = 0usize;

    for (path, health, fixes, effective, velocity) in components {
        let id = Uuid::new_v4().to_string();
        conn.execute(
            r#"INSERT INTO component_health_snapshots
               (id, workflow_name, component_path, health_score, fix_count,
                effective_fix_count, change_velocity, snapshot_at)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, datetime('now'))"#,
            params![id, workflow_name, path, health, fixes, effective, velocity],
        )
        .map_err(|e| format!("Failed to insert component health snapshot: {}", e))?;
        count += 1;
    }

    Ok(count)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};
    use rusqlite::Connection;

    fn setup_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE convergence_snapshots (
                id TEXT PRIMARY KEY,
                workflow_name TEXT NOT NULL,
                project_path TEXT,
                scope TEXT NOT NULL DEFAULT 'workflow',
                convergence_score REAL NOT NULL,
                consecutive_clean_runs INTEGER NOT NULL,
                novelty_score REAL NOT NULL,
                effective_fix_rate REAL NOT NULL,
                change_velocity REAL NOT NULL,
                total_fixes INTEGER NOT NULL,
                effective_fixes INTEGER NOT NULL,
                snapshot_at TEXT NOT NULL
            );
            CREATE TABLE component_health_snapshots (
                id TEXT PRIMARY KEY,
                workflow_name TEXT NOT NULL,
                component_path TEXT NOT NULL,
                health_score REAL NOT NULL,
                fix_count INTEGER NOT NULL DEFAULT 0,
                effective_fix_count INTEGER NOT NULL DEFAULT 0,
                change_velocity REAL NOT NULL DEFAULT 0.0,
                snapshot_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            "#,
        )
        .unwrap();
        conn
    }

    fn insert_convergence_snapshot(
        conn: &Connection,
        workflow: &str,
        score: f64,
        fix_rate: f64,
        velocity: f64,
        total: i64,
        effective: i64,
        at: &str,
    ) {
        let id = Uuid::new_v4().to_string();
        conn.execute(
            r#"INSERT INTO convergence_snapshots
               (id, workflow_name, scope, convergence_score, consecutive_clean_runs,
                novelty_score, effective_fix_rate, change_velocity, total_fixes,
                effective_fixes, snapshot_at)
               VALUES (?1, ?2, 'workflow', ?3, 0, 0.0, ?4, ?5, ?6, ?7, ?8)"#,
            params![id, workflow, score, fix_rate, velocity, total, effective, at],
        )
        .unwrap();
    }

    #[test]
    fn test_workflow_trends_empty() {
        let conn = setup_test_db();
        let trends = get_workflow_trends(&conn, "nonexistent", None).unwrap();
        assert_eq!(trends.snapshot_count, 0);
        assert!(trends.convergence.is_empty());
    }

    #[test]
    fn test_workflow_trends_with_data() {
        let conn = setup_test_db();
        insert_convergence_snapshot(
            &conn,
            "test-wf",
            0.5,
            0.8,
            1.2,
            10,
            8,
            "2026-03-01T00:00:00Z",
        );
        insert_convergence_snapshot(
            &conn,
            "test-wf",
            0.7,
            0.9,
            0.8,
            15,
            13,
            "2026-03-02T00:00:00Z",
        );
        insert_convergence_snapshot(
            &conn,
            "test-wf",
            0.85,
            0.95,
            0.3,
            20,
            19,
            "2026-03-03T00:00:00Z",
        );

        let trends = get_workflow_trends(&conn, "test-wf", None).unwrap();
        assert_eq!(trends.snapshot_count, 3);
        assert_eq!(trends.convergence.len(), 3);
        assert!((trends.convergence[0].value - 0.5).abs() < 0.001);
        assert!((trends.convergence[2].value - 0.85).abs() < 0.001);
        assert!((trends.fix_rate[1].value - 0.9).abs() < 0.001);
    }

    #[test]
    fn test_workflow_trends_time_filter() {
        let conn = setup_test_db();
        let recent = (Utc::now() - Duration::hours(12))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        insert_convergence_snapshot(
            &conn,
            "test-wf",
            0.3,
            0.5,
            2.0,
            5,
            2,
            "2020-01-01T00:00:00Z",
        );
        insert_convergence_snapshot(&conn, "test-wf", 0.8, 0.9, 0.5, 15, 13, &recent);

        let trends = get_workflow_trends(&conn, "test-wf", Some("7d")).unwrap();
        // Only the recent snapshot should pass the filter
        assert_eq!(trends.snapshot_count, 1);
        assert!((trends.convergence[0].value - 0.8).abs() < 0.001);
    }

    #[test]
    fn test_workflow_trends_all_range() {
        let conn = setup_test_db();
        let recent = (Utc::now() - Duration::days(2))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        insert_convergence_snapshot(
            &conn,
            "test-wf",
            0.3,
            0.5,
            2.0,
            5,
            2,
            "2020-01-01T00:00:00Z",
        );
        insert_convergence_snapshot(&conn, "test-wf", 0.8, 0.9, 0.5, 15, 13, &recent);

        let trends = get_workflow_trends(&conn, "test-wf", Some("all")).unwrap();
        assert_eq!(trends.snapshot_count, 2);
    }

    #[test]
    fn test_component_trend_empty() {
        let conn = setup_test_db();
        let trend = get_component_trend(&conn, "wf", "src/main.rs", None).unwrap();
        assert!(trend.health_scores.is_empty());
        assert!(trend.fix_counts.is_empty());
    }

    #[test]
    fn test_store_and_query_component_health() {
        let conn = setup_test_db();
        let components = vec![
            ("src/main.rs".to_string(), 0.85, 5, 4, 0.3),
            ("src/lib.rs".to_string(), 0.6, 10, 6, 1.2),
        ];
        let stored = store_component_health_snapshots(&conn, "test-wf", &components).unwrap();
        assert_eq!(stored, 2);

        let trend = get_component_trend(&conn, "test-wf", "src/main.rs", None).unwrap();
        assert_eq!(trend.health_scores.len(), 1);
        assert!((trend.health_scores[0].value - 0.85).abs() < 0.001);
        assert_eq!(trend.component_path, "src/main.rs");
    }

    #[test]
    fn test_effectiveness_over_time_empty() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE task_runs (id TEXT PRIMARY KEY, workflow_name TEXT NOT NULL DEFAULT '');
            CREATE TABLE reflection_fixes (
                id TEXT PRIMARY KEY,
                source_task_run_id TEXT,
                effectiveness TEXT,
                evaluated_at TEXT
            );
            "#,
        )
        .unwrap();
        let result = get_effectiveness_over_time(&conn, "nonexistent", "week", None).unwrap();
        assert!(result.buckets.is_empty());
    }

    #[test]
    fn test_effectiveness_over_time_bucketing() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE task_runs (id TEXT PRIMARY KEY, workflow_name TEXT NOT NULL DEFAULT '');
            CREATE TABLE reflection_fixes (
                id TEXT PRIMARY KEY,
                source_task_run_id TEXT,
                effectiveness TEXT,
                evaluated_at TEXT
            );
            INSERT INTO task_runs (id, workflow_name) VALUES ('tr1', 'test-wf');
            INSERT INTO reflection_fixes (id, source_task_run_id, effectiveness, evaluated_at)
                VALUES ('f1', 'tr1', 'effective', '2026-03-02T10:00:00Z');
            INSERT INTO reflection_fixes (id, source_task_run_id, effectiveness, evaluated_at)
                VALUES ('f2', 'tr1', 'effective', '2026-03-03T10:00:00Z');
            INSERT INTO reflection_fixes (id, source_task_run_id, effectiveness, evaluated_at)
                VALUES ('f3', 'tr1', 'ineffective', '2026-03-04T12:00:00Z');
            INSERT INTO reflection_fixes (id, source_task_run_id, effectiveness, evaluated_at)
                VALUES ('f4', 'tr1', 'regression', '2026-04-10T10:00:00Z');
            "#,
        )
        .unwrap();

        // Monthly bucketing to avoid week-boundary issues
        let result = get_effectiveness_over_time(&conn, "test-wf", "month", Some("all")).unwrap();
        assert!(
            !result.buckets.is_empty(),
            "Should have at least one bucket"
        );
        // March bucket should contain f1, f2, f3
        let march = result
            .buckets
            .iter()
            .find(|b| b.bucket == "2026-03")
            .unwrap();
        assert_eq!(march.total, 3, "March bucket should have 3 fixes");
        assert_eq!(march.effective, 2);
        assert_eq!(march.ineffective, 1);
        assert_eq!(march.regression, 0);
        assert!((march.effectiveness_rate - 2.0 / 3.0).abs() < 0.001);
        // April bucket should have regression
        let april = result
            .buckets
            .iter()
            .find(|b| b.bucket == "2026-04")
            .unwrap();
        assert_eq!(april.regression, 1);
    }

    #[test]
    fn test_effectiveness_over_time_monthly() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE task_runs (id TEXT PRIMARY KEY, workflow_name TEXT NOT NULL DEFAULT '');
            CREATE TABLE reflection_fixes (
                id TEXT PRIMARY KEY,
                source_task_run_id TEXT,
                effectiveness TEXT,
                evaluated_at TEXT
            );
            INSERT INTO task_runs (id, workflow_name) VALUES ('tr1', 'test-wf');
            INSERT INTO reflection_fixes (id, source_task_run_id, effectiveness, evaluated_at)
                VALUES ('f1', 'tr1', 'effective', '2026-02-15T10:00:00Z');
            INSERT INTO reflection_fixes (id, source_task_run_id, effectiveness, evaluated_at)
                VALUES ('f2', 'tr1', 'effective', '2026-03-15T10:00:00Z');
            "#,
        )
        .unwrap();

        let result = get_effectiveness_over_time(&conn, "test-wf", "month", Some("all")).unwrap();
        assert_eq!(result.buckets.len(), 2, "Should have 2 monthly buckets");
        assert_eq!(result.buckets[0].bucket, "2026-02");
        assert_eq!(result.buckets[1].bucket, "2026-03");
        assert!((result.buckets[0].effectiveness_rate - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_parse_time_cutoff() {
        assert!(parse_time_cutoff(None).is_none());
        assert!(parse_time_cutoff(Some("all")).is_none());
        assert!(parse_time_cutoff(Some("invalid")).is_none());
        assert!(parse_time_cutoff(Some("7d")).is_some());
        assert!(parse_time_cutoff(Some("24h")).is_some());
        assert!(parse_time_cutoff(Some("30d")).is_some());
    }
}
