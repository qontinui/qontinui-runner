//! Debug context curator for aggregating error data.
//!
//! This module provides:
//! - Aggregation of errors from multiple sources (error_events, automation output)
//! - Pattern detection across errors
//! - Formatted context for the debug agent
//! - Priority scoring for errors

use std::collections::HashMap;

use crate::error_monitor::types::{ErrorSeverity, ErrorStatus, StoredErrorEvent};

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
    /// Descriptions of log sources referenced by errors (source name -> description)
    #[serde(default)]
    pub source_descriptions: HashMap<String, String>,
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
    /// Errors with similar embeddings (semantic similarity)
    SimilarEmbedding,
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
        score += error.occurrence_count.min(20) * 3;

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
        patterns.sort_by_key(|p| std::cmp::Reverse(p.frequency));
        patterns
    }

    /// Detect embedding-based similarity patterns.
    /// This requires database access to read the message_embedding column.
    pub fn detect_embedding_patterns(&self, error_ids: &[i64]) -> Vec<ErrorPattern> {
        Vec::new()
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

    /// Check if a curated error is a UI Bridge spec verification failure.
    /// Spec errors have messages starting with "SPEC: " and come from Runner Actions.
    fn is_spec_error(error: &CuratedError) -> bool {
        error.message.contains("SPEC: ") && error.source == "Runner Actions"
    }

    /// Classify errors in a DebugContext as spec failures vs runtime errors.
    /// Returns (spec_count, runtime_count).
    pub fn classify_errors(context: &DebugContext) -> (usize, usize) {
        let all_errors = context
            .critical_errors
            .iter()
            .chain(context.errors.iter())
            .chain(context.warnings.iter());

        let spec_count = all_errors
            .clone()
            .filter(|e| Self::is_spec_error(e))
            .count();
        let total = all_errors.count();
        (spec_count, total - spec_count)
    }

    /// Format the debug context as a string for AI consumption.
    pub fn format_for_ai(&self, context: &DebugContext) -> String {
        let mut output = String::new();

        output.push_str("# Error Analysis Report\n\n");

        // Add source descriptions with context-aware messaging
        if !context.source_descriptions.is_empty() {
            let (spec_count, runtime_count) = Self::classify_errors(context);

            output.push_str("## Log Sources\n\n");

            if spec_count > 0 && runtime_count == 0 {
                // ALL errors are spec failures
                output.push_str("These errors are **UI Bridge spec verification failures**, NOT application runtime errors.\n\n");
                output.push_str("UI Bridge specs are assertion-based tests defined in `.spec.uibridge.json` files that verify ");
                output.push_str(
                    "the state of UI elements (existence, text content, visibility, attributes). ",
                );
                output
                    .push_str("A spec failure means the UI doesn't match the expected state.\n\n");
                output.push_str("To fix spec failures:\n");
                output.push_str(
                    "1. Find the `.spec.uibridge.json` file containing the spec definition\n",
                );
                output.push_str("2. Check the `assertions` array to understand what's expected\n");
                output.push_str("3. Determine whether the **app code** needs to change (UI not rendering correctly) ");
                output.push_str(
                    "or the **spec definition** needs updating (expectations are outdated)\n",
                );
                output.push_str("4. Do NOT look for Playwright tests or other test frameworks — these are UI Bridge specs\n\n");
            } else if spec_count > 0 && runtime_count > 0 {
                // MIXED: some spec, some runtime
                output.push_str("These errors include BOTH application runtime errors and UI Bridge spec verification failures.\n\n");
                output.push_str("**Runtime errors** come from application logs — fix the application code that produces them.\n");
                output.push_str("**Spec failures** (messages starting with `SPEC: `) are UI Bridge assertion failures from ");
                output.push_str("`.spec.uibridge.json` files — check whether the app UI or the spec definition needs updating.\n\n");
            } else {
                // NO spec failures — all runtime errors (original behavior)
                output.push_str("These errors come from the following monitored log sources. ");
                output.push_str("These are APPLICATION RUNTIME logs, NOT test failures. ");
                output.push_str("Fix the application code that produces these errors.\n\n");
            }

            for (name, description) in &context.source_descriptions {
                output.push_str(&format!("- **{}**: {}\n", name, description));
            }
            output.push('\n');
        }

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
