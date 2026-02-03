//! Storage operations for findings in SQLite.
//!
//! Provides CRUD operations and queries for the task_run_findings table.

#![allow(dead_code)]

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use tracing::{debug, info};

use super::types::{
    Finding, FindingActionType, FindingCategory, FindingCodeContext, FindingSeverity,
    FindingStatus, FindingSummary, FindingUserInput, ParsedFinding,
};

/// Insert a new finding into the database
pub fn insert_finding(
    conn: &Connection,
    task_run_id: &str,
    session_num: u32,
    parsed: &ParsedFinding,
) -> Result<Finding, String> {
    let now = Utc::now().to_rfc3339();
    let id = uuid::Uuid::new_v4().to_string();

    // Determine action type based on needs_input
    let action_type = if parsed.needs_input {
        FindingActionType::NeedsUserInput
    } else {
        // Could be more sophisticated - check category for informational
        match parsed.category {
            FindingCategory::AlreadyFixed | FindingCategory::ExpectedBehavior => {
                FindingActionType::Informational
            }
            _ => FindingActionType::AutoFix,
        }
    };

    // Compute signature hash for deduplication
    let signature_hash = compute_signature_hash(
        &parsed.category,
        &parsed.title,
        parsed.file.as_deref(),
        parsed.line,
    );

    // Check for existing finding with same signature
    if let Some(existing) = find_by_signature(conn, task_run_id, &signature_hash)? {
        // If existing finding is terminal (resolved, wont_fix), don't create duplicate
        if existing.status.is_terminal() {
            debug!(
                "Skipping duplicate finding '{}' - already resolved",
                parsed.title
            );
            return Ok(existing);
        }

        // Update existing finding with new session info
        debug!(
            "Updating existing finding '{}' with new detection",
            parsed.title
        );
        // Could update description or other fields here if desired
        return Ok(existing);
    }

    // Serialize options as JSON if present
    let input_options_json = parsed
        .options
        .as_ref()
        .map(|opts| serde_json::to_string(opts).unwrap_or_default());

    // Determine initial status based on modifiers
    let initial_status = if parsed.is_resolved {
        "resolved"
    } else if parsed.needs_input {
        "needs_input"
    } else {
        "detected"
    };

    // For resolved findings, use the current timestamp as resolved_at
    let resolved_at = if parsed.is_resolved {
        Some(now.clone())
    } else {
        None
    };

    // For resolved findings, record the session where it was resolved
    let resolved_in_session = if parsed.is_resolved {
        Some(session_num)
    } else {
        None
    };

    conn.execute(
        r#"
        INSERT INTO task_run_findings (
            id, task_run_id, category, severity, signature_hash,
            title, description, file_path, line_number,
            status, action_type, resolution,
            detected_in_session, needs_input, question, input_options,
            detected_at, resolved_at, resolved_in_session, updated_at
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5,
            ?6, ?7, ?8, ?9,
            ?10, ?11, ?12,
            ?13, ?14, ?15, ?16,
            ?17, ?18, ?19, ?20
        )
        "#,
        params![
            id,
            task_run_id,
            parsed.category.as_str(),
            parsed.severity.as_str(),
            signature_hash,
            parsed.title,
            parsed.description,
            parsed.file,
            parsed.line,
            initial_status,
            action_type.as_str(),
            parsed.resolution,
            session_num,
            parsed.needs_input,
            parsed.question,
            input_options_json,
            now,
            resolved_at,
            resolved_in_session,
            now,
        ],
    )
    .map_err(|e| format!("Failed to insert finding: {}", e))?;

    info!(
        "Inserted finding '{}' for task {}",
        parsed.title, task_run_id
    );

    // Return the created finding
    get_finding(conn, &id)?.ok_or_else(|| "Finding not found after insert".to_string())
}

/// Find a finding by signature hash (for deduplication)
pub fn find_by_signature(
    conn: &Connection,
    task_run_id: &str,
    signature_hash: &str,
) -> Result<Option<Finding>, String> {
    let result = conn
        .query_row(
            r#"
        SELECT id, task_run_id, detected_in_session, category, severity, status,
               action_type, title, description, resolution, file_path, line_number,
               column_number, code_snippet, signature_hash, needs_input, question,
               input_options, user_response, detected_at, resolved_at,
               resolved_in_session, updated_at
        FROM task_run_findings
        WHERE task_run_id = ?1 AND signature_hash = ?2
        "#,
            params![task_run_id, signature_hash],
            row_to_finding,
        )
        .optional()
        .map_err(|e| format!("Failed to find by signature: {}", e))?;

    Ok(result)
}

/// Get a finding by ID
pub fn get_finding(conn: &Connection, id: &str) -> Result<Option<Finding>, String> {
    let result = conn
        .query_row(
            r#"
        SELECT id, task_run_id, detected_in_session, category, severity, status,
               action_type, title, description, resolution, file_path, line_number,
               column_number, code_snippet, signature_hash, needs_input, question,
               input_options, user_response, detected_at, resolved_at,
               resolved_in_session, updated_at
        FROM task_run_findings
        WHERE id = ?1
        "#,
            params![id],
            row_to_finding,
        )
        .optional()
        .map_err(|e| format!("Failed to get finding: {}", e))?;

    Ok(result)
}

/// Get all findings for a task run
pub fn get_findings_for_task(conn: &Connection, task_run_id: &str) -> Result<Vec<Finding>, String> {
    let mut stmt = conn
        .prepare(
            r#"
        SELECT id, task_run_id, detected_in_session, category, severity, status,
               action_type, title, description, resolution, file_path, line_number,
               column_number, code_snippet, signature_hash, needs_input, question,
               input_options, user_response, detected_at, resolved_at,
               resolved_in_session, updated_at
        FROM task_run_findings
        WHERE task_run_id = ?1
        ORDER BY detected_at ASC
        "#,
        )
        .map_err(|e| format!("Failed to prepare query: {}", e))?;

    let findings = stmt
        .query_map(params![task_run_id], row_to_finding)
        .map_err(|e| format!("Failed to query findings: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(findings)
}

/// Get findings by status for a task run
pub fn get_findings_by_status(
    conn: &Connection,
    task_run_id: &str,
    status: &FindingStatus,
) -> Result<Vec<Finding>, String> {
    let mut stmt = conn
        .prepare(
            r#"
        SELECT id, task_run_id, detected_in_session, category, severity, status,
               action_type, title, description, resolution, file_path, line_number,
               column_number, code_snippet, signature_hash, needs_input, question,
               input_options, user_response, detected_at, resolved_at,
               resolved_in_session, updated_at
        FROM task_run_findings
        WHERE task_run_id = ?1 AND status = ?2
        ORDER BY detected_at ASC
        "#,
        )
        .map_err(|e| format!("Failed to prepare query: {}", e))?;

    let findings = stmt
        .query_map(params![task_run_id, status.as_str()], row_to_finding)
        .map_err(|e| format!("Failed to query findings: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(findings)
}

/// Update finding status
pub fn update_finding_status(
    conn: &Connection,
    id: &str,
    status: &FindingStatus,
    resolution: Option<&str>,
    session_num: Option<u32>,
) -> Result<(), String> {
    let now = Utc::now().to_rfc3339();
    let resolved_at = if status.is_terminal() {
        Some(now.clone())
    } else {
        None
    };

    conn.execute(
        r#"
        UPDATE task_run_findings
        SET status = ?2, resolution = ?3, resolved_at = ?4,
            resolved_in_session = ?5, updated_at = ?6
        WHERE id = ?1
        "#,
        params![
            id,
            status.as_str(),
            resolution,
            resolved_at,
            session_num,
            now
        ],
    )
    .map_err(|e| format!("Failed to update finding: {}", e))?;

    Ok(())
}

/// Set user response for a finding
pub fn set_user_response(conn: &Connection, id: &str, response: &str) -> Result<(), String> {
    let now = Utc::now().to_rfc3339();

    conn.execute(
        r#"
        UPDATE task_run_findings
        SET user_response = ?2, status = 'in_progress', updated_at = ?3
        WHERE id = ?1
        "#,
        params![id, response, now],
    )
    .map_err(|e| format!("Failed to set user response: {}", e))?;

    Ok(())
}

/// Format findings for inclusion in a continuation prompt.
///
/// This creates a structured section showing:
/// - Resolved findings (so AI doesn't re-report them)
/// - Outstanding findings (still need to be addressed)
/// - Findings needing user input (with any responses)
///
/// This prevents the AI from re-detecting already-resolved issues and
/// provides context about what work remains.
pub fn format_findings_for_continuation_prompt(
    conn: &Connection,
    task_run_id: &str,
) -> Result<String, String> {
    let findings = get_findings_for_task(conn, task_run_id)?;

    if findings.is_empty() {
        return Ok(String::new());
    }

    let mut sections = Vec::new();
    sections.push("## Previous Session Findings\n".to_string());

    // Group findings by status
    let mut resolved: Vec<&Finding> = Vec::new();
    let mut outstanding: Vec<&Finding> = Vec::new();
    let mut needs_input: Vec<&Finding> = Vec::new();
    let mut with_responses: Vec<&Finding> = Vec::new();

    for finding in &findings {
        // Check if this finding has a user response (regardless of current status)
        // User responses are set when user answers a needs_input question
        if finding.user_response.is_some() {
            with_responses.push(finding);
            continue;
        }

        match finding.status {
            FindingStatus::Resolved | FindingStatus::WontFix | FindingStatus::Deferred => {
                resolved.push(finding);
            }
            FindingStatus::NeedsInput => {
                needs_input.push(finding);
            }
            FindingStatus::Detected | FindingStatus::InProgress => {
                outstanding.push(finding);
            }
        }
    }

    // Section: Resolved findings (DO NOT re-report)
    if !resolved.is_empty() {
        sections.push("### Already Resolved (DO NOT re-report these)\n".to_string());
        for f in &resolved {
            let location = format_location(&f.code_context);
            let resolution = f.resolution.as_deref().unwrap_or("(no details)");
            sections.push(format!(
                "- **{}** [{}]{}\n  Resolution: {}\n",
                f.title,
                f.status.as_str(),
                location,
                resolution
            ));
        }
        sections.push(String::new());
    }

    // Section: User responses (context for continuation)
    if !with_responses.is_empty() {
        sections.push("### User Responses Provided\n".to_string());
        for f in &with_responses {
            let question = f
                .user_input
                .as_ref()
                .map(|ui| ui.question.as_str())
                .unwrap_or("(question not available)");
            let response = f.user_response.as_deref().unwrap_or("(no response)");
            sections.push(format!(
                "- **{}**\n  Question: {}\n  Response: {}\n",
                f.title, question, response
            ));
        }
        sections.push(String::new());
    }

    // Section: Outstanding findings (still need attention)
    if !outstanding.is_empty() {
        sections.push("### Outstanding Findings (still need attention)\n".to_string());
        for f in &outstanding {
            let location = format_location(&f.code_context);
            sections.push(format!(
                "- **[{}:{}]** {}{}\n  {}\n",
                f.category.as_str(),
                f.severity.as_str(),
                f.title,
                location,
                truncate_description(&f.description, 200)
            ));
        }
        sections.push(String::new());
    }

    // Section: Awaiting user input (blocked)
    if !needs_input.is_empty() {
        sections.push("### Awaiting User Input (blocked until response)\n".to_string());
        for f in &needs_input {
            let question = f
                .user_input
                .as_ref()
                .map(|ui| ui.question.as_str())
                .unwrap_or("(question not available)");
            let options = f
                .user_input
                .as_ref()
                .and_then(|ui| ui.options.as_ref())
                .map(|opts| format!(" Options: {}", opts.join(", ")))
                .unwrap_or_default();
            sections.push(format!(
                "- **{}**\n  Question: {}{}\n",
                f.title, question, options
            ));
        }
        sections.push(String::new());
    }

    // Summary
    sections.push(format!(
        "**Summary:** {} total findings - {} resolved, {} outstanding, {} awaiting input, {} with responses\n",
        findings.len(),
        resolved.len(),
        outstanding.len(),
        needs_input.len(),
        with_responses.len()
    ));

    Ok(sections.join(""))
}

/// Format code location for display
fn format_location(code_context: &Option<FindingCodeContext>) -> String {
    match code_context {
        Some(ctx) => {
            let file = ctx.file.as_deref().unwrap_or("");
            let line = ctx.line.map(|l| format!(":{}", l)).unwrap_or_default();
            if file.is_empty() {
                String::new()
            } else {
                format!(" @ `{}{}`", file, line)
            }
        }
        None => String::new(),
    }
}

/// Truncate description to max length, adding ellipsis if needed
fn truncate_description(desc: &str, max_len: usize) -> String {
    if desc.len() <= max_len {
        desc.to_string()
    } else {
        format!("{}...", &desc[..max_len])
    }
}

/// Get summary statistics for a task run
pub fn get_finding_summary(conn: &Connection, task_run_id: &str) -> Result<FindingSummary, String> {
    let findings = get_findings_for_task(conn, task_run_id)?;

    let mut summary = FindingSummary {
        task_run_id: task_run_id.to_string(),
        total: findings.len() as u32,
        ..Default::default()
    };

    for finding in &findings {
        // Count by category
        *summary
            .by_category
            .entry(finding.category.as_str().to_string())
            .or_insert(0) += 1;

        // Count by severity
        *summary
            .by_severity
            .entry(finding.severity.as_str().to_string())
            .or_insert(0) += 1;

        // Count by status
        *summary
            .by_status
            .entry(finding.status.as_str().to_string())
            .or_insert(0) += 1;

        // Count special statuses
        if finding.status == FindingStatus::NeedsInput {
            summary.needs_input_count += 1;
        }
        if finding.status == FindingStatus::Resolved {
            summary.resolved_count += 1;
        }
        if !finding.status.is_terminal() && finding.status != FindingStatus::NeedsInput {
            summary.outstanding_count += 1;
        }
    }

    Ok(summary)
}

/// Compute a signature hash for deduplication
fn compute_signature_hash(
    category: &FindingCategory,
    title: &str,
    file: Option<&str>,
    line: Option<u32>,
) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    category.as_str().hash(&mut hasher);
    title.hash(&mut hasher);
    file.hash(&mut hasher);
    line.hash(&mut hasher);
    format!("{:x}", hasher.finish())
}

/// Convert a database row to a Finding
fn row_to_finding(row: &rusqlite::Row) -> rusqlite::Result<Finding> {
    let category_str: String = row.get(3)?;
    let severity_str: String = row.get(4)?;
    let status_str: String = row.get(5)?;
    let action_type_str: String = row.get(6)?;

    let file_path: Option<String> = row.get(10)?;
    let line_number: Option<u32> = row.get(11)?;
    let column_number: Option<u32> = row.get(12)?;
    let code_snippet: Option<String> = row.get(13)?;

    let code_context = if file_path.is_some() || line_number.is_some() {
        Some(FindingCodeContext {
            file: file_path,
            line: line_number,
            column: column_number,
            snippet: code_snippet,
        })
    } else {
        None
    };

    let needs_input: bool = row.get(15)?;
    let question: Option<String> = row.get(16)?;
    let input_options_json: Option<String> = row.get(17)?;

    let user_input = if needs_input && question.is_some() {
        let options =
            input_options_json.and_then(|json| serde_json::from_str::<Vec<String>>(&json).ok());
        Some(FindingUserInput {
            question: question.unwrap(),
            input_type: if options.is_some() {
                "choice".to_string()
            } else {
                "text".to_string()
            },
            options,
        })
    } else {
        None
    };

    let signature_hash: Option<String> = row.get(14)?;

    Ok(Finding {
        id: row.get(0)?,
        task_run_id: row.get(1)?,
        session_num: row.get(2)?,
        category: FindingCategory::from_str(&category_str).unwrap_or(FindingCategory::CodeBug),
        severity: FindingSeverity::from_str(&severity_str).unwrap_or(FindingSeverity::Medium),
        status: FindingStatus::from_str(&status_str).unwrap_or(FindingStatus::Detected),
        action_type: FindingActionType::from_str(&action_type_str)
            .unwrap_or(FindingActionType::AutoFix),
        title: row.get(7)?,
        description: row.get(8)?,
        resolution: row.get(9)?,
        code_context,
        signature_hash: signature_hash.unwrap_or_default(),
        user_input,
        user_response: row.get(18)?,
        detected_at: row.get(19)?,
        resolved_at: row.get(20)?,
        resolved_in_session: row.get(21)?,
        updated_at: row.get(22)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn create_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
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
                detected_in_session INTEGER NOT NULL,
                resolved_in_session INTEGER,
                needs_input BOOLEAN DEFAULT 0,
                question TEXT,
                input_options TEXT,
                user_response TEXT,
                detected_at TEXT NOT NULL,
                resolved_at TEXT,
                updated_at TEXT NOT NULL
            );
            "#,
        )
        .unwrap();
        conn
    }

    #[test]
    fn test_insert_and_get_finding() {
        let conn = create_test_db();

        let parsed = ParsedFinding {
            category: FindingCategory::CodeBug,
            severity: FindingSeverity::High,
            needs_input: false,
            is_resolved: false,
            title: "Test bug".to_string(),
            description: "A test bug description".to_string(),
            file: Some("src/main.rs".to_string()),
            line: Some(42),
            resolution: None,
            question: None,
            options: None,
        };

        let finding = insert_finding(&conn, "task-123", 1, &parsed).unwrap();

        assert_eq!(finding.title, "Test bug");
        assert_eq!(finding.task_run_id, "task-123");
        assert_eq!(finding.session_num, 1);
        assert_eq!(finding.category, FindingCategory::CodeBug);
        assert_eq!(finding.severity, FindingSeverity::High);
        assert_eq!(finding.status, FindingStatus::Detected);

        // Get by ID
        let loaded = get_finding(&conn, &finding.id).unwrap().unwrap();
        assert_eq!(loaded.id, finding.id);
        assert_eq!(loaded.title, "Test bug");
    }

    #[test]
    fn test_deduplication() {
        let conn = create_test_db();

        let parsed = ParsedFinding {
            category: FindingCategory::CodeBug,
            severity: FindingSeverity::High,
            needs_input: false,
            is_resolved: false,
            title: "Duplicate bug".to_string(),
            description: "First description".to_string(),
            file: Some("src/lib.rs".to_string()),
            line: Some(10),
            resolution: None,
            question: None,
            options: None,
        };

        let first = insert_finding(&conn, "task-456", 1, &parsed).unwrap();
        let second = insert_finding(&conn, "task-456", 2, &parsed).unwrap();

        // Should return the same finding (deduplication)
        assert_eq!(first.id, second.id);

        // Only one finding should exist
        let all = get_findings_for_task(&conn, "task-456").unwrap();
        assert_eq!(all.len(), 1);
    }

    #[test]
    fn test_update_status() {
        let conn = create_test_db();

        let parsed = ParsedFinding {
            category: FindingCategory::Security,
            severity: FindingSeverity::Critical,
            needs_input: false,
            is_resolved: false,
            title: "Security issue".to_string(),
            description: "Critical security bug".to_string(),
            file: None,
            line: None,
            resolution: None,
            question: None,
            options: None,
        };

        let finding = insert_finding(&conn, "task-789", 1, &parsed).unwrap();
        assert_eq!(finding.status, FindingStatus::Detected);

        // Update to resolved
        update_finding_status(
            &conn,
            &finding.id,
            &FindingStatus::Resolved,
            Some("Fixed the vulnerability"),
            Some(2),
        )
        .unwrap();

        let updated = get_finding(&conn, &finding.id).unwrap().unwrap();
        assert_eq!(updated.status, FindingStatus::Resolved);
        assert_eq!(
            updated.resolution,
            Some("Fixed the vulnerability".to_string())
        );
        assert_eq!(updated.resolved_in_session, Some(2));
        assert!(updated.resolved_at.is_some());
    }

    #[test]
    fn test_finding_summary() {
        let conn = create_test_db();

        // Insert multiple findings
        let findings = vec![
            ParsedFinding {
                category: FindingCategory::CodeBug,
                severity: FindingSeverity::High,
                needs_input: false,
                is_resolved: false,
                title: "Bug 1".to_string(),
                description: "First bug".to_string(),
                file: Some("a.rs".to_string()),
                line: Some(1),
                resolution: None,
                question: None,
                options: None,
            },
            ParsedFinding {
                category: FindingCategory::Security,
                severity: FindingSeverity::Critical,
                needs_input: true,
                is_resolved: false,
                title: "Security 1".to_string(),
                description: "Security issue".to_string(),
                file: Some("b.rs".to_string()),
                line: Some(2),
                resolution: None,
                question: Some("How to fix?".to_string()),
                options: Some(vec!["Option A".to_string(), "Option B".to_string()]),
            },
            ParsedFinding {
                category: FindingCategory::CodeBug,
                severity: FindingSeverity::Medium,
                needs_input: false,
                is_resolved: false,
                title: "Bug 2".to_string(),
                description: "Second bug".to_string(),
                file: Some("c.rs".to_string()),
                line: Some(3),
                resolution: None,
                question: None,
                options: None,
            },
        ];

        for (i, parsed) in findings.iter().enumerate() {
            insert_finding(&conn, "task-summary", (i + 1) as u32, parsed).unwrap();
        }

        let summary = get_finding_summary(&conn, "task-summary").unwrap();

        assert_eq!(summary.total, 3);
        assert_eq!(summary.by_category.get("code_bug"), Some(&2));
        assert_eq!(summary.by_category.get("security"), Some(&1));
        assert_eq!(summary.needs_input_count, 1);
        assert_eq!(summary.outstanding_count, 2); // 2 detected, not needs_input
    }

    #[test]
    fn test_user_response() {
        let conn = create_test_db();

        let parsed = ParsedFinding {
            category: FindingCategory::Todo,
            severity: FindingSeverity::Low,
            needs_input: true,
            is_resolved: false,
            title: "Question".to_string(),
            description: "Need user input".to_string(),
            file: None,
            line: None,
            resolution: None,
            question: Some("What should we do?".to_string()),
            options: Some(vec!["Fix it".to_string(), "Ignore it".to_string()]),
        };

        let finding = insert_finding(&conn, "task-input", 1, &parsed).unwrap();
        assert_eq!(finding.status, FindingStatus::NeedsInput);

        set_user_response(&conn, &finding.id, "Fix it").unwrap();

        let updated = get_finding(&conn, &finding.id).unwrap().unwrap();
        assert_eq!(updated.status, FindingStatus::InProgress);
        assert_eq!(updated.user_response, Some("Fix it".to_string()));
    }

    #[test]
    fn test_insert_resolved_finding() {
        let conn = create_test_db();

        let parsed = ParsedFinding {
            category: FindingCategory::CodeBug,
            severity: FindingSeverity::High,
            needs_input: false,
            is_resolved: true,
            title: "Fixed bug".to_string(),
            description: "This bug was fixed".to_string(),
            file: Some("src/main.rs".to_string()),
            line: Some(42),
            resolution: Some("Added null check".to_string()),
            question: None,
            options: None,
        };

        let finding = insert_finding(&conn, "task-resolved", 1, &parsed).unwrap();

        // Should be marked as resolved immediately
        assert_eq!(finding.status, FindingStatus::Resolved);
        assert_eq!(finding.resolution, Some("Added null check".to_string()));
        assert!(finding.resolved_at.is_some());
        assert_eq!(finding.resolved_in_session, Some(1));
    }

    #[test]
    fn test_continuation_prompt_formatting() {
        let conn = create_test_db();
        let task_id = "task-prompt";

        // Insert findings with different statuses
        let resolved_finding = ParsedFinding {
            category: FindingCategory::CodeBug,
            severity: FindingSeverity::High,
            needs_input: false,
            is_resolved: true,
            title: "Fixed null pointer".to_string(),
            description: "Fixed a null pointer bug".to_string(),
            file: Some("src/main.rs".to_string()),
            line: Some(42),
            resolution: Some("Added null check".to_string()),
            question: None,
            options: None,
        };
        insert_finding(&conn, task_id, 1, &resolved_finding).unwrap();

        let outstanding_finding = ParsedFinding {
            category: FindingCategory::Security,
            severity: FindingSeverity::Critical,
            needs_input: false,
            is_resolved: false,
            title: "SQL injection risk".to_string(),
            description: "Potential SQL injection in user query".to_string(),
            file: Some("src/db.rs".to_string()),
            line: Some(100),
            resolution: None,
            question: None,
            options: None,
        };
        insert_finding(&conn, task_id, 1, &outstanding_finding).unwrap();

        let needs_input_finding = ParsedFinding {
            category: FindingCategory::Enhancement,
            severity: FindingSeverity::Medium,
            needs_input: true,
            is_resolved: false,
            title: "Caching strategy".to_string(),
            description: "Should we add caching here?".to_string(),
            file: None,
            line: None,
            resolution: None,
            question: Some("What caching backend should we use?".to_string()),
            options: Some(vec!["Redis".to_string(), "Memcached".to_string()]),
        };
        insert_finding(&conn, task_id, 1, &needs_input_finding).unwrap();

        // Generate the continuation prompt section
        let prompt = format_findings_for_continuation_prompt(&conn, task_id).unwrap();

        // Verify the prompt contains expected sections
        assert!(prompt.contains("## Previous Session Findings"));
        assert!(prompt.contains("### Already Resolved"));
        assert!(prompt.contains("Fixed null pointer"));
        assert!(prompt.contains("Added null check"));
        assert!(prompt.contains("### Outstanding Findings"));
        assert!(prompt.contains("SQL injection risk"));
        assert!(prompt.contains("### Awaiting User Input"));
        assert!(prompt.contains("Caching strategy"));
        assert!(prompt.contains("What caching backend should we use?"));
        assert!(prompt.contains("Redis, Memcached"));
        assert!(prompt.contains("**Summary:**"));
    }

    #[test]
    fn test_continuation_prompt_empty() {
        let conn = create_test_db();

        // No findings
        let prompt = format_findings_for_continuation_prompt(&conn, "task-empty").unwrap();
        assert!(prompt.is_empty());
    }

    #[test]
    fn test_continuation_prompt_with_user_response() {
        let conn = create_test_db();
        let task_id = "task-responses";

        // Insert a finding that needs input
        let finding = ParsedFinding {
            category: FindingCategory::Todo,
            severity: FindingSeverity::Low,
            needs_input: true,
            is_resolved: false,
            title: "API design choice".to_string(),
            description: "Need to decide on API design".to_string(),
            file: None,
            line: None,
            resolution: None,
            question: Some("REST or GraphQL?".to_string()),
            options: Some(vec!["REST".to_string(), "GraphQL".to_string()]),
        };
        let f = insert_finding(&conn, task_id, 1, &finding).unwrap();

        // Provide user response
        set_user_response(&conn, &f.id, "REST").unwrap();

        // Generate prompt - should show user responses section
        let prompt = format_findings_for_continuation_prompt(&conn, task_id).unwrap();

        assert!(prompt.contains("### User Responses Provided"));
        assert!(prompt.contains("API design choice"));
        assert!(prompt.contains("REST or GraphQL?"));
        assert!(prompt.contains("Response: REST"));
    }
}
