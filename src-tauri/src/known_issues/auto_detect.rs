//! Auto-detection of recurring findings that should become known issues.
//!
//! When the same finding (by `signature_hash`) recurs across multiple
//! distinct task runs, this module automatically promotes it to a
//! persistent `KnownIssue`.

use rusqlite::Connection;
use tracing::{debug, info, warn};

use crate::known_issues::storage::insert_known_issue;
use crate::known_issues::types::{
    CreateKnownIssueRequest, DetectionMethod, IssueCategory, IssueProvenance, IssueSeverity,
    ScopeType,
};

/// Check findings from the current task run and promote any recurring ones
/// (appearing in 2+ distinct task runs) to known issues.
///
/// Returns the IDs of any newly created known issues.
pub fn check_and_promote_recurring_findings(
    conn: &Connection,
    task_run_id: &str,
) -> Result<Vec<String>, String> {
    let mut new_issue_ids = Vec::new();

    // 1. Query findings from the current task run that have a signature_hash
    let mut stmt = conn
        .prepare(
            r#"
            SELECT id, category, severity, title, description, signature_hash, file_path
            FROM task_run_findings
            WHERE task_run_id = ?1 AND signature_hash IS NOT NULL
            "#,
        )
        .map_err(|e| format!("Failed to prepare findings query: {}", e))?;

    struct FindingRow {
        id: String,
        category: String,
        severity: String,
        title: String,
        description: String,
        signature_hash: String,
        file_path: Option<String>,
    }

    let findings: Vec<FindingRow> = stmt
        .query_map(rusqlite::params![task_run_id], |row| {
            Ok(FindingRow {
                id: row.get(0)?,
                category: row.get(1)?,
                severity: row.get(2)?,
                title: row.get(3)?,
                description: row.get(4)?,
                signature_hash: row.get(5)?,
                file_path: row.get(6)?,
            })
        })
        .map_err(|e| format!("Failed to query findings: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    if findings.is_empty() {
        debug!(
            "No findings with signature_hash in task run {}",
            task_run_id
        );
        return Ok(new_issue_ids);
    }

    debug!(
        "Checking {} finding(s) for recurrence in task run {}",
        findings.len(),
        task_run_id
    );

    for finding in &findings {
        // 2. Count distinct task runs with the same signature hash
        let recurrence_count: u32 = conn
            .query_row(
                "SELECT COUNT(DISTINCT task_run_id) FROM task_run_findings WHERE signature_hash = ?1",
                rusqlite::params![finding.signature_hash],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to count recurrences: {}", e))?;

        if recurrence_count < 2 {
            continue;
        }

        // 3. Check if a known issue already exists for this signature hash
        let existing_count: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM known_issues WHERE json_extract(detection_config, '$.signature_hash') = ?1",
                rusqlite::params![finding.signature_hash],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to check existing known issues: {}", e))?;

        if existing_count > 0 {
            debug!(
                "Known issue already exists for signature_hash={} (finding '{}')",
                finding.signature_hash, finding.title
            );
            continue;
        }

        // 4. Collect all finding IDs with this signature hash
        let mut id_stmt = conn
            .prepare("SELECT id FROM task_run_findings WHERE signature_hash = ?1")
            .map_err(|e| format!("Failed to prepare source finding IDs query: {}", e))?;

        let source_finding_ids: Vec<String> = id_stmt
            .query_map(rusqlite::params![finding.signature_hash], |row| row.get(0))
            .map_err(|e| format!("Failed to query source finding IDs: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        // Map FindingCategory -> IssueCategory
        let issue_category = map_finding_category(&finding.category);

        // Map FindingSeverity -> IssueSeverity
        let issue_severity = map_finding_severity(&finding.severity);

        // Confidence based on recurrence count
        let confidence = match recurrence_count {
            2 => 0.6,
            3 => 0.8,
            _ => 0.9,
        };

        // Build verification hint from description (first 200 chars)
        let verification_hint = if finding.description.len() > 200 {
            Some(finding.description[..200].to_string())
        } else {
            Some(finding.description.clone())
        };

        // Build detection_config with signature_hash and source finding IDs
        let detection_config = serde_json::json!({
            "signature_hash": finding.signature_hash,
            "source_finding_ids": source_finding_ids,
        });

        let req = CreateKnownIssueRequest {
            title: finding.title.clone(),
            description: format!(
                "Auto-detected recurring finding (seen in {} task runs). Original: {}",
                recurrence_count, finding.description
            ),
            category: issue_category,
            scope_type: ScopeType::Global,
            scope_value: finding.file_path.clone(),
            scope_tags: None,
            detection_method: DetectionMethod::AiJudgment,
            detection_config: Some(detection_config),
            pattern_template_id: None,
            reproduction_context: None,
            trigger_conditions: None,
            severity: issue_severity,
            provenance: Some(IssueProvenance::AutoDetected),
            source_finding_ids: Some(source_finding_ids),
            source_task_run_id: Some(task_run_id.to_string()),
            verification_hint,
            verification_step_template: None,
        };

        match insert_known_issue(conn, &req) {
            Ok(issue) => {
                info!(
                    "Auto-promoted recurring finding '{}' to known issue '{}' (seen in {} runs, confidence={:.1})",
                    finding.title, issue.id, recurrence_count, confidence
                );
                new_issue_ids.push(issue.id);
            }
            Err(e) => {
                warn!(
                    "Failed to create known issue from recurring finding '{}': {}",
                    finding.title, e
                );
            }
        }
    }

    Ok(new_issue_ids)
}

/// Map a finding category string to an IssueCategory.
fn map_finding_category(category: &str) -> IssueCategory {
    match category {
        "performance" => IssueCategory::Performance,
        "runtime_issue" => IssueCategory::State,
        "security" => IssueCategory::Other,
        "code_bug" => IssueCategory::Other,
        "config_issue" => IssueCategory::Other,
        "test_issue" => IssueCategory::Other,
        "todo" => IssueCategory::Other,
        "enhancement" => IssueCategory::Other,
        "documentation" => IssueCategory::Other,
        "already_fixed" => IssueCategory::Other,
        "expected_behavior" => IssueCategory::Other,
        "warning" => IssueCategory::Other,
        "data_migration" => IssueCategory::DataIntegrity,
        _ => IssueCategory::Other,
    }
}

/// Map a finding severity string to an IssueSeverity.
fn map_finding_severity(severity: &str) -> IssueSeverity {
    match severity {
        "critical" => IssueSeverity::Critical,
        "high" => IssueSeverity::High,
        "medium" => IssueSeverity::Medium,
        "low" => IssueSeverity::Low,
        "info" => IssueSeverity::Low,
        _ => IssueSeverity::Medium,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::known_issues::storage::ensure_tables;

    fn create_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        ensure_tables(&conn).unwrap();
        // Create the task_run_findings table for testing
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS task_run_findings (
                id TEXT PRIMARY KEY,
                task_run_id TEXT NOT NULL,
                category TEXT NOT NULL,
                severity TEXT NOT NULL,
                signature_hash TEXT,
                title TEXT NOT NULL,
                description TEXT NOT NULL,
                file_path TEXT,
                line_number INTEGER,
                column_number INTEGER,
                code_snippet TEXT,
                status TEXT NOT NULL DEFAULT 'detected',
                action_type TEXT NOT NULL DEFAULT 'auto_fix',
                resolution TEXT,
                detected_in_session INTEGER NOT NULL DEFAULT 0,
                resolved_in_session INTEGER,
                needs_input BOOLEAN DEFAULT 0,
                question TEXT,
                input_options TEXT,
                user_response TEXT
            );
            "#,
        )
        .unwrap();
        conn
    }

    #[test]
    fn test_no_findings() {
        let conn = create_test_db();
        let result = check_and_promote_recurring_findings(&conn, "task-1").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_single_occurrence_not_promoted() {
        let conn = create_test_db();
        conn.execute(
            "INSERT INTO task_run_findings (id, task_run_id, category, severity, signature_hash, title, description) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params!["f1", "task-1", "code_bug", "high", "hash-abc", "Bug A", "Description A"],
        ).unwrap();

        let result = check_and_promote_recurring_findings(&conn, "task-1").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_recurring_finding_promoted() {
        let conn = create_test_db();
        // Same signature_hash in two different task runs
        conn.execute(
            "INSERT INTO task_run_findings (id, task_run_id, category, severity, signature_hash, title, description) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params!["f1", "task-1", "code_bug", "high", "hash-abc", "Bug A", "Description A"],
        ).unwrap();
        conn.execute(
            "INSERT INTO task_run_findings (id, task_run_id, category, severity, signature_hash, title, description) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params!["f2", "task-2", "code_bug", "high", "hash-abc", "Bug A", "Description A"],
        ).unwrap();

        let result = check_and_promote_recurring_findings(&conn, "task-1").unwrap();
        assert_eq!(result.len(), 1);

        // Verify the known issue was created
        let issue = crate::known_issues::storage::get_known_issue(&conn, &result[0])
            .unwrap()
            .unwrap();
        assert_eq!(issue.title, "Bug A");
        assert_eq!(issue.provenance, IssueProvenance::AutoDetected);
        assert_eq!(issue.severity, IssueSeverity::High);
    }

    #[test]
    fn test_duplicate_not_created() {
        let conn = create_test_db();
        conn.execute(
            "INSERT INTO task_run_findings (id, task_run_id, category, severity, signature_hash, title, description) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params!["f1", "task-1", "code_bug", "high", "hash-abc", "Bug A", "Description A"],
        ).unwrap();
        conn.execute(
            "INSERT INTO task_run_findings (id, task_run_id, category, severity, signature_hash, title, description) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params!["f2", "task-2", "code_bug", "high", "hash-abc", "Bug A", "Description A"],
        ).unwrap();

        // First call creates a known issue
        let result1 = check_and_promote_recurring_findings(&conn, "task-1").unwrap();
        assert_eq!(result1.len(), 1);

        // Second call should NOT create a duplicate
        let result2 = check_and_promote_recurring_findings(&conn, "task-1").unwrap();
        assert!(result2.is_empty());
    }

    #[test]
    fn test_category_mapping() {
        assert_eq!(
            map_finding_category("performance"),
            IssueCategory::Performance
        );
        assert_eq!(map_finding_category("runtime_issue"), IssueCategory::State);
        assert_eq!(
            map_finding_category("data_migration"),
            IssueCategory::DataIntegrity
        );
        assert_eq!(map_finding_category("code_bug"), IssueCategory::Other);
        assert_eq!(map_finding_category("unknown"), IssueCategory::Other);
    }

    #[test]
    fn test_severity_mapping() {
        assert_eq!(map_finding_severity("critical"), IssueSeverity::Critical);
        assert_eq!(map_finding_severity("high"), IssueSeverity::High);
        assert_eq!(map_finding_severity("medium"), IssueSeverity::Medium);
        assert_eq!(map_finding_severity("low"), IssueSeverity::Low);
        assert_eq!(map_finding_severity("info"), IssueSeverity::Low);
    }
}
