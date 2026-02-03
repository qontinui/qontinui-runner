//! Debug context curator for aggregating error data.
//!
//! This module provides:
//! - Aggregation of errors from multiple sources (error_events, automation output)
//! - Pattern detection across errors
//! - Formatted context for the debug agent
//! - Priority scoring for errors

use std::collections::HashMap;

use rusqlite::Connection;

use crate::error_monitor::storage::ErrorEventStorage;
use crate::error_monitor::types::{ErrorQuery, ErrorSeverity, ErrorStatus, StoredErrorEvent};

/// A curated collection of errors ready for debug agent analysis.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DebugContext {
    /// Summary of the error situation
    pub summary: String,
    /// High-priority errors that need immediate attention
    pub critical_errors: Vec<CuratedError>,
    /// Regular errors
    pub errors: Vec<CuratedError>,
    /// Warnings (lower priority)
    pub warnings: Vec<CuratedError>,
    /// Detected patterns across errors
    pub patterns: Vec<ErrorPattern>,
    /// Suggested focus areas
    pub focus_areas: Vec<String>,
    /// Total error count
    pub total_count: u32,
    /// Whether immediate action is recommended
    pub requires_immediate_action: bool,
}

/// A curated error with additional context for debugging.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CuratedError {
    /// Original error event ID
    pub id: i64,
    /// Error type (e.g., "TypeError", "ConnectionError")
    pub error_type: Option<String>,
    /// Error message
    pub message: String,
    /// File location if available
    pub location: Option<String>,
    /// Stack trace excerpt (first few frames)
    pub stack_excerpt: Option<String>,
    /// Number of occurrences
    pub occurrence_count: u32,
    /// Source of the error (log source name)
    pub source: String,
    /// Priority score (higher = more important)
    pub priority_score: u32,
    /// Suggested investigation steps
    pub investigation_hints: Vec<String>,
    /// Related error IDs (similar errors)
    pub related_errors: Vec<i64>,
}

/// A detected pattern across multiple errors.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ErrorPattern {
    /// Pattern name/description
    pub name: String,
    /// Error IDs that match this pattern
    pub error_ids: Vec<i64>,
    /// Frequency (how many errors match)
    pub frequency: u32,
    /// Pattern type
    pub pattern_type: PatternType,
    /// Suggested root cause
    pub suggested_cause: Option<String>,
}

/// Types of error patterns.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PatternType {
    /// Same error type occurring multiple times
    RepeatedErrorType,
    /// Errors in the same file
    SameFileLocation,
    /// Errors from the same source
    SameSource,
    /// Errors with similar messages
    SimilarMessage,
    /// Cascading failures (one error causing others)
    CascadingFailure,
}

/// Configuration for the debug context curator.
#[derive(Debug, Clone)]
pub struct CuratorConfig {
    /// Maximum number of errors to include in context
    pub max_errors: usize,
    /// Maximum stack trace lines to include
    pub max_stack_lines: usize,
    /// Minimum occurrence count to consider a pattern
    pub min_pattern_frequency: u32,
    /// Whether to include resolved errors for context
    pub include_resolved: bool,
}

impl Default for CuratorConfig {
    fn default() -> Self {
        Self {
            max_errors: 50,
            max_stack_lines: 5,
            min_pattern_frequency: 2,
            include_resolved: false,
        }
    }
}

/// The debug context curator.
pub struct DebugContextCurator {
    config: CuratorConfig,
}

impl DebugContextCurator {
    /// Create a new curator with default configuration.
    pub fn new() -> Self {
        Self {
            config: CuratorConfig::default(),
        }
    }

    /// Create a curator with custom configuration.
    pub fn with_config(config: CuratorConfig) -> Self {
        Self { config }
    }

    /// Build debug context from the database.
    pub fn build_context(
        &self,
        conn: &Connection,
        task_run_id: Option<&str>,
    ) -> Result<DebugContext, String> {
        // Query errors based on configuration
        let statuses = if self.config.include_resolved {
            None
        } else {
            Some(vec![
                ErrorStatus::New,
                ErrorStatus::Acknowledged,
                ErrorStatus::InProgress,
                ErrorStatus::Promoted,
            ])
        };

        let query = ErrorQuery {
            task_run_id: task_run_id.map(|s| s.to_string()),
            status: statuses,
            limit: Some(self.config.max_errors as u32),
            ..Default::default()
        };

        let errors = ErrorEventStorage::query(conn, &query)?;

        if errors.is_empty() {
            return Ok(DebugContext {
                summary: "No errors found.".to_string(),
                critical_errors: vec![],
                errors: vec![],
                warnings: vec![],
                patterns: vec![],
                focus_areas: vec![],
                total_count: 0,
                requires_immediate_action: false,
            });
        }

        // Categorize and curate errors
        let mut critical_errors = Vec::new();
        let mut regular_errors = Vec::new();
        let mut warnings = Vec::new();

        for error in &errors {
            let curated = self.curate_error(error, &errors);
            match error.severity {
                ErrorSeverity::Critical => critical_errors.push(curated),
                ErrorSeverity::Error => regular_errors.push(curated),
                ErrorSeverity::Warning => warnings.push(curated),
                ErrorSeverity::Info => {} // Info-level events are not errors, skip them
            }
        }

        // Sort by priority score
        critical_errors.sort_by(|a, b| b.priority_score.cmp(&a.priority_score));
        regular_errors.sort_by(|a, b| b.priority_score.cmp(&a.priority_score));
        warnings.sort_by(|a, b| b.priority_score.cmp(&a.priority_score));

        // Detect patterns
        let patterns = self.detect_patterns(&errors);

        // Generate focus areas
        let focus_areas = self.generate_focus_areas(&critical_errors, &regular_errors, &patterns);

        // Generate summary
        let summary =
            self.generate_summary(&critical_errors, &regular_errors, &warnings, &patterns);

        let requires_immediate_action =
            !critical_errors.is_empty() || regular_errors.iter().any(|e| e.occurrence_count >= 5);

        Ok(DebugContext {
            summary,
            critical_errors,
            errors: regular_errors,
            warnings,
            patterns,
            focus_areas,
            total_count: errors.len() as u32,
            requires_immediate_action,
        })
    }

    /// Curate a single error with additional context.
    fn curate_error(
        &self,
        error: &StoredErrorEvent,
        all_errors: &[StoredErrorEvent],
    ) -> CuratedError {
        // Extract location string
        let location = error.location.as_ref().map(|loc| {
            let mut s = loc.file_path.clone();
            if let Some(line) = loc.line_number {
                s.push_str(&format!(":{}", line));
            }
            if let Some(func) = &loc.function_name {
                s.push_str(&format!(" in {}", func));
            }
            s
        });

        // Extract stack excerpt
        let stack_excerpt = error.stack_trace.as_ref().map(|st| {
            st.lines()
                .take(self.config.max_stack_lines)
                .collect::<Vec<_>>()
                .join("\n")
        });

        // Calculate priority score
        let priority_score = self.calculate_priority_score(error);

        // Generate investigation hints
        let investigation_hints = self.generate_investigation_hints(error);

        // Find related errors
        let related_errors = self.find_related_errors(error, all_errors);

        CuratedError {
            id: error.id,
            error_type: error.error_type.clone(),
            message: error.message.clone(),
            location,
            stack_excerpt,
            occurrence_count: error.occurrence_count,
            source: error.log_source_name.clone(),
            priority_score,
            investigation_hints,
            related_errors,
        }
    }

    /// Calculate priority score for an error.
    fn calculate_priority_score(&self, error: &StoredErrorEvent) -> u32 {
        let mut score = 0u32;

        // Severity contributes most
        match error.severity {
            ErrorSeverity::Critical => score += 100,
            ErrorSeverity::Error => score += 50,
            ErrorSeverity::Warning => score += 10,
            ErrorSeverity::Info => score += 1,
        }

        // Occurrence count matters
        score += (error.occurrence_count.min(20) * 3) as u32;

        // Newer errors are more relevant
        if error.status == ErrorStatus::New {
            score += 20;
        }

        // Errors with stack traces are easier to debug
        if error.stack_trace.is_some() {
            score += 10;
        }

        // Errors with location info are easier to find
        if error.location.is_some() {
            score += 5;
        }

        score
    }

    /// Generate investigation hints for an error.
    fn generate_investigation_hints(&self, error: &StoredErrorEvent) -> Vec<String> {
        let mut hints = Vec::new();

        // Type-specific hints
        if let Some(ref error_type) = error.error_type {
            match error_type.as_str() {
                "TypeError" | "AttributeError" => {
                    hints.push("Check for None/null values or incorrect types".to_string());
                }
                "KeyError" | "IndexError" => {
                    hints.push("Verify the key/index exists before accessing".to_string());
                }
                "ConnectionError" | "TimeoutError" => {
                    hints.push("Check network connectivity and service availability".to_string());
                }
                "ImportError" | "ModuleNotFoundError" => {
                    hints.push("Verify the module is installed and in the Python path".to_string());
                }
                "SyntaxError" | "IndentationError" => {
                    hints.push("Check for syntax issues in the specified file".to_string());
                }
                "PermissionError" => {
                    hints.push("Check file/directory permissions".to_string());
                }
                "FileNotFoundError" => {
                    hints.push("Verify the file path exists".to_string());
                }
                _ => {}
            }
        }

        // Location-based hints
        if let Some(ref loc) = error.location {
            hints.push(format!("Start investigation at {}", loc.file_path));
            if let Some(line) = loc.line_number {
                hints.push(format!("Focus on line {} and surrounding context", line));
            }
        }

        // Occurrence-based hints
        if error.occurrence_count > 5 {
            hints.push(format!(
                "This error has occurred {} times - likely a systematic issue",
                error.occurrence_count
            ));
        }

        hints
    }

    /// Find errors related to a given error.
    fn find_related_errors(
        &self,
        error: &StoredErrorEvent,
        all_errors: &[StoredErrorEvent],
    ) -> Vec<i64> {
        let mut related = Vec::new();

        for other in all_errors {
            if other.id == error.id {
                continue;
            }

            // Same error type
            if error.error_type.is_some() && error.error_type == other.error_type {
                related.push(other.id);
                continue;
            }

            // Same file
            if let (Some(ref loc1), Some(ref loc2)) = (&error.location, &other.location) {
                if loc1.file_path == loc2.file_path {
                    related.push(other.id);
                    continue;
                }
            }

            // Similar message (simple substring match)
            if error.message.len() > 10 && other.message.contains(&error.message[..10]) {
                related.push(other.id);
            }
        }

        // Limit to top 5 related
        related.truncate(5);
        related
    }

    /// Detect patterns across errors.
    fn detect_patterns(&self, errors: &[StoredErrorEvent]) -> Vec<ErrorPattern> {
        let mut patterns = Vec::new();

        // Pattern: Same error type
        let mut by_type: HashMap<String, Vec<i64>> = HashMap::new();
        for error in errors {
            if let Some(ref error_type) = error.error_type {
                by_type
                    .entry(error_type.clone())
                    .or_default()
                    .push(error.id);
            }
        }
        for (error_type, ids) in by_type {
            if ids.len() >= self.config.min_pattern_frequency as usize {
                patterns.push(ErrorPattern {
                    name: format!("Repeated {} errors", error_type),
                    error_ids: ids.clone(),
                    frequency: ids.len() as u32,
                    pattern_type: PatternType::RepeatedErrorType,
                    suggested_cause: Some(format!(
                        "Multiple {} errors suggest a common root cause",
                        error_type
                    )),
                });
            }
        }

        // Pattern: Same file location
        let mut by_file: HashMap<String, Vec<i64>> = HashMap::new();
        for error in errors {
            if let Some(ref loc) = error.location {
                by_file
                    .entry(loc.file_path.clone())
                    .or_default()
                    .push(error.id);
            }
        }
        for (file_path, ids) in by_file {
            if ids.len() >= self.config.min_pattern_frequency as usize {
                patterns.push(ErrorPattern {
                    name: format!("Multiple errors in {}", file_path),
                    error_ids: ids.clone(),
                    frequency: ids.len() as u32,
                    pattern_type: PatternType::SameFileLocation,
                    suggested_cause: Some(format!(
                        "File {} may have structural issues or bugs",
                        file_path
                    )),
                });
            }
        }

        // Pattern: Same source
        let mut by_source: HashMap<String, Vec<i64>> = HashMap::new();
        for error in errors {
            by_source
                .entry(error.log_source_name.clone())
                .or_default()
                .push(error.id);
        }
        for (source, ids) in by_source {
            if ids.len() >= self.config.min_pattern_frequency as usize * 2 {
                // Higher threshold for source patterns
                patterns.push(ErrorPattern {
                    name: format!("Concentration of errors in {}", source),
                    error_ids: ids.clone(),
                    frequency: ids.len() as u32,
                    pattern_type: PatternType::SameSource,
                    suggested_cause: Some(format!("The {} component may need attention", source)),
                });
            }
        }

        // Sort patterns by frequency
        patterns.sort_by(|a, b| b.frequency.cmp(&a.frequency));
        patterns
    }

    /// Generate suggested focus areas based on errors and patterns.
    fn generate_focus_areas(
        &self,
        critical_errors: &[CuratedError],
        errors: &[CuratedError],
        patterns: &[ErrorPattern],
    ) -> Vec<String> {
        let mut areas = Vec::new();

        // Critical errors are always focus areas
        for error in critical_errors.iter().take(3) {
            if let Some(ref loc) = error.location {
                areas.push(format!("CRITICAL: {}", loc));
            } else if let Some(ref error_type) = error.error_type {
                areas.push(format!(
                    "CRITICAL: {} - {}",
                    error_type,
                    &error.message[..50.min(error.message.len())]
                ));
            }
        }

        // High-frequency patterns
        for pattern in patterns.iter().take(3) {
            if pattern.frequency >= 3 {
                areas.push(pattern.name.clone());
            }
        }

        // High-occurrence errors
        for error in errors.iter().filter(|e| e.occurrence_count >= 5).take(3) {
            areas.push(format!(
                "Recurring: {} ({} times)",
                error.message.chars().take(40).collect::<String>(),
                error.occurrence_count
            ));
        }

        areas.truncate(5);
        areas
    }

    /// Generate a human-readable summary.
    fn generate_summary(
        &self,
        critical_errors: &[CuratedError],
        errors: &[CuratedError],
        warnings: &[CuratedError],
        patterns: &[ErrorPattern],
    ) -> String {
        let mut parts = Vec::new();

        let total = critical_errors.len() + errors.len() + warnings.len();

        if critical_errors.is_empty() && errors.is_empty() {
            if warnings.is_empty() {
                return "No issues found.".to_string();
            } else {
                return format!("{} warning(s) detected.", warnings.len());
            }
        }

        parts.push(format!("Found {} issue(s):", total));

        if !critical_errors.is_empty() {
            parts.push(format!("  - {} CRITICAL", critical_errors.len()));
        }
        if !errors.is_empty() {
            parts.push(format!("  - {} errors", errors.len()));
        }
        if !warnings.is_empty() {
            parts.push(format!("  - {} warnings", warnings.len()));
        }

        if !patterns.is_empty() {
            parts.push(format!(
                "Detected {} pattern(s) that may indicate systemic issues.",
                patterns.len()
            ));
        }

        parts.join("\n")
    }

    /// Format the debug context as a string for AI consumption.
    pub fn format_for_ai(&self, context: &DebugContext) -> String {
        let mut output = String::new();

        output.push_str("# Error Analysis Report\n\n");
        output.push_str(&format!("## Summary\n{}\n\n", context.summary));

        if context.requires_immediate_action {
            output.push_str("⚠️ **IMMEDIATE ACTION REQUIRED**\n\n");
        }

        if !context.focus_areas.is_empty() {
            output.push_str("## Focus Areas\n");
            for area in &context.focus_areas {
                output.push_str(&format!("- {}\n", area));
            }
            output.push('\n');
        }

        if !context.critical_errors.is_empty() {
            output.push_str("## Critical Errors\n");
            for error in &context.critical_errors {
                self.format_error_for_ai(&mut output, error);
            }
            output.push('\n');
        }

        if !context.errors.is_empty() {
            output.push_str("## Errors\n");
            for error in context.errors.iter().take(10) {
                self.format_error_for_ai(&mut output, error);
            }
            if context.errors.len() > 10 {
                output.push_str(&format!(
                    "... and {} more errors\n",
                    context.errors.len() - 10
                ));
            }
            output.push('\n');
        }

        if !context.patterns.is_empty() {
            output.push_str("## Detected Patterns\n");
            for pattern in &context.patterns {
                output.push_str(&format!(
                    "- **{}** ({} occurrences)\n",
                    pattern.name, pattern.frequency
                ));
                if let Some(ref cause) = pattern.suggested_cause {
                    output.push_str(&format!("  Suggested cause: {}\n", cause));
                }
            }
            output.push('\n');
        }

        output
    }

    /// Format a single error for AI consumption.
    fn format_error_for_ai(&self, output: &mut String, error: &CuratedError) {
        output.push_str(&format!(
            "### {}\n",
            error.error_type.as_deref().unwrap_or("Error")
        ));
        output.push_str(&format!("**Message:** {}\n", error.message));

        if let Some(ref loc) = error.location {
            output.push_str(&format!("**Location:** {}\n", loc));
        }

        output.push_str(&format!("**Occurrences:** {}\n", error.occurrence_count));
        output.push_str(&format!("**Source:** {}\n", error.source));

        if let Some(ref stack) = error.stack_excerpt {
            output.push_str("**Stack trace:**\n```\n");
            output.push_str(stack);
            output.push_str("\n```\n");
        }

        if !error.investigation_hints.is_empty() {
            output.push_str("**Investigation hints:**\n");
            for hint in &error.investigation_hints {
                output.push_str(&format!("- {}\n", hint));
            }
        }

        output.push('\n');
    }
}

impl Default for DebugContextCurator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error_monitor::types::ErrorLocation;

    fn make_test_error(
        id: i64,
        severity: ErrorSeverity,
        error_type: &str,
        message: &str,
    ) -> StoredErrorEvent {
        StoredErrorEvent {
            id,
            log_source_id: Some(1),
            log_source_name: "test".to_string(),
            task_run_id: None,
            workflow_step_id: None,
            log_timestamp: None,
            captured_at: "2024-01-15T10:00:00Z".to_string(),
            severity,
            error_type: Some(error_type.to_string()),
            error_code: None,
            message: message.to_string(),
            stack_trace: None,
            context_lines: None,
            raw_entry: None,
            location: Some(ErrorLocation {
                file_path: "/app/main.py".to_string(),
                line_number: Some(42),
                column_number: None,
                function_name: Some("process".to_string()),
            }),
            signature_hash: format!("hash_{}", id),
            occurrence_count: 1,
            first_seen_at: "2024-01-15T10:00:00Z".to_string(),
            last_seen_at: "2024-01-15T10:00:00Z".to_string(),
            status: ErrorStatus::New,
            finding_id: None,
            resolved_by_task_run_id: None,
            resolution_notes: None,
            acknowledged_at: None,
            resolved_at: None,
        }
    }

    #[test]
    fn test_priority_scoring() {
        let curator = DebugContextCurator::new();

        let critical = make_test_error(1, ErrorSeverity::Critical, "CriticalError", "msg");
        let error = make_test_error(2, ErrorSeverity::Error, "Error", "msg");
        let warning = make_test_error(3, ErrorSeverity::Warning, "Warning", "msg");

        let critical_score = curator.calculate_priority_score(&critical);
        let error_score = curator.calculate_priority_score(&error);
        let warning_score = curator.calculate_priority_score(&warning);

        assert!(critical_score > error_score);
        assert!(error_score > warning_score);
    }

    #[test]
    fn test_pattern_detection() {
        let curator = DebugContextCurator::new();

        let errors = vec![
            make_test_error(1, ErrorSeverity::Error, "TypeError", "msg1"),
            make_test_error(2, ErrorSeverity::Error, "TypeError", "msg2"),
            make_test_error(3, ErrorSeverity::Error, "TypeError", "msg3"),
            make_test_error(4, ErrorSeverity::Error, "KeyError", "msg4"),
        ];

        let patterns = curator.detect_patterns(&errors);

        // Should detect the repeated TypeError pattern
        assert!(patterns.iter().any(|p| p.name.contains("TypeError")));
    }

    #[test]
    fn test_investigation_hints() {
        let curator = DebugContextCurator::new();

        let error = make_test_error(
            1,
            ErrorSeverity::Error,
            "TypeError",
            "cannot add str and int",
        );
        let hints = curator.generate_investigation_hints(&error);

        assert!(!hints.is_empty());
        assert!(hints
            .iter()
            .any(|h| h.contains("None/null") || h.contains("types")));
    }
}
