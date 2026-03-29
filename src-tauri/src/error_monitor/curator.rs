//! Debug context curator for aggregating error data.
//!
//! This module provides:
//! - Aggregation of errors from multiple sources (error_events, automation output)
//! - Pattern detection across errors
//! - Formatted context for the debug agent
//! - Priority scoring for errors

use std::collections::HashMap;

use rusqlite::Connection;

use crate::error_monitor::storage::{ErrorEventStorage, LogSourceStorage};
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
                source_descriptions: HashMap::new(),
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

        // Enrich investigation hints with past resolutions from hybrid search
        Self::enrich_with_past_resolutions(conn, &mut critical_errors);
        Self::enrich_with_past_resolutions(conn, &mut regular_errors);

        // Sort by priority score
        critical_errors.sort_by(|a, b| b.priority_score.cmp(&a.priority_score));
        regular_errors.sort_by(|a, b| b.priority_score.cmp(&a.priority_score));
        warnings.sort_by(|a, b| b.priority_score.cmp(&a.priority_score));

        // Detect patterns
        let mut patterns = self.detect_patterns(&errors);

        // Detect embedding-based similarity patterns
        let error_ids: Vec<i64> = errors.iter().map(|e| e.id).collect();
        let embedding_patterns = self.detect_embedding_patterns(conn, &error_ids);
        patterns.extend(embedding_patterns);

        // Generate focus areas
        let focus_areas = self.generate_focus_areas(&critical_errors, &regular_errors, &patterns);

        // Generate summary
        let summary =
            self.generate_summary(&critical_errors, &regular_errors, &warnings, &patterns);

        let requires_immediate_action =
            !critical_errors.is_empty() || regular_errors.iter().any(|e| e.occurrence_count >= 5);

        // Build source descriptions from log_sources table
        let source_descriptions = Self::build_source_descriptions(conn, &errors);

        Ok(DebugContext {
            summary,
            critical_errors,
            errors: regular_errors,
            warnings,
            patterns,
            focus_areas,
            total_count: errors.len() as u32,
            requires_immediate_action,
            source_descriptions,
        })
    }

    /// Build a map of source name -> description from the log_sources table.
    fn build_source_descriptions(
        conn: &Connection,
        errors: &[StoredErrorEvent],
    ) -> HashMap<String, String> {
        // Collect unique source names from errors
        let unique_sources: std::collections::HashSet<&str> =
            errors.iter().map(|e| e.log_source_name.as_str()).collect();

        let mut descriptions = HashMap::new();

        // Query log sources from DB to get their descriptions
        if let Ok(log_sources) = LogSourceStorage::get_all(conn) {
            for source in &log_sources {
                if unique_sources.contains(source.name.as_str()) {
                    if let Some(ref desc) = source.description {
                        if !desc.is_empty() {
                            descriptions.insert(source.name.clone(), desc.clone());
                        }
                    }
                }
            }
        }

        descriptions
    }

    /// Enrich curated errors with past resolution hints from the findings database.
    ///
    /// For each critical/error-level error, queries task_run_findings for resolved
    /// entries with similar messages and appends the resolution approach to
    /// investigation_hints. Falls back gracefully if no matches are found.
    fn enrich_with_past_resolutions(conn: &Connection, errors: &mut [CuratedError]) {
        for error in errors.iter_mut() {
            // Search for resolved findings with similar messages
            let sql = r#"
                SELECT title, resolution
                FROM task_run_findings
                WHERE status = 'resolved'
                  AND resolution IS NOT NULL
                  AND (title LIKE ?1 OR description LIKE ?1)
                ORDER BY resolved_at DESC
                LIMIT 2
            "#;

            // Use a substring of the error message for matching
            let search_term = if error.message.len() > 80 {
                format!("%{}%", &error.message[..80])
            } else {
                format!("%{}%", error.message)
            };

            if let Ok(mut stmt) = conn.prepare(sql) {
                let hints: Vec<String> = stmt
                    .query_map(rusqlite::params![search_term], |row| {
                        let title: String = row.get(0)?;
                        let resolution: String = row.get(1)?;
                        Ok(format!("Past resolution for '{}': {}", title, resolution))
                    })
                    .ok()
                    .map(|rows| rows.filter_map(|r| r.ok()).collect())
                    .unwrap_or_default();

                if !hints.is_empty() {
                    error.investigation_hints.extend(hints);
                }
            }
        }
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
        patterns.sort_by(|a, b| b.frequency.cmp(&a.frequency));
        patterns
    }

    /// Detect embedding-based similarity patterns.
    /// This requires database access to read the message_embedding column.
    pub fn detect_embedding_patterns(
        &self,
        conn: &Connection,
        error_ids: &[i64],
    ) -> Vec<ErrorPattern> {
        if error_ids.is_empty() {
            return Vec::new();
        }

        // Guard: skip O(n^2) pairwise comparison when the set is too large
        if error_ids.len() > 200 {
            tracing::debug!(
                "Skipping embedding clustering: {} error_ids exceeds 200 limit",
                error_ids.len()
            );
            return Vec::new();
        }

        // Build a parameterized IN clause
        let placeholders: Vec<String> = (1..=error_ids.len()).map(|i| format!("?{}", i)).collect();
        let sql = format!(
            "SELECT id, message, message_embedding FROM error_events WHERE id IN ({}) AND message_embedding IS NOT NULL",
            placeholders.join(", ")
        );

        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };

        // Bind parameters
        let params: Vec<&dyn rusqlite::types::ToSql> = error_ids
            .iter()
            .map(|id| id as &dyn rusqlite::types::ToSql)
            .collect();

        let rows: Vec<(i64, String, Vec<f32>)> = match stmt.query_map(params.as_slice(), |row| {
            let id: i64 = row.get(0)?;
            let message: String = row.get(1)?;
            let blob: Vec<u8> = row.get(2)?;
            // Parse embedding: 384 f32 values, little-endian
            let embedding: Vec<f32> = blob
                .chunks_exact(4)
                .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
                .collect();
            Ok((id, message, embedding))
        }) {
            Ok(mapped) => mapped.filter_map(|r| r.ok()).collect(),
            Err(_) => return Vec::new(),
        };

        if rows.len() < 2 {
            return Vec::new();
        }

        // Precompute norms
        let norms: Vec<f32> = rows
            .iter()
            .map(|(_, _, emb)| {
                let sum_sq: f32 = emb.iter().map(|x| x * x).sum();
                sum_sq.sqrt().max(1e-10)
            })
            .collect();

        // Greedy clustering with cosine similarity threshold
        let threshold: f32 = 0.85;
        let mut assigned = vec![false; rows.len()];
        let mut clusters: Vec<Vec<usize>> = Vec::new();

        for i in 0..rows.len() {
            if assigned[i] {
                continue;
            }
            let mut cluster = vec![i];
            assigned[i] = true;

            for j in (i + 1)..rows.len() {
                if assigned[j] {
                    continue;
                }
                // Cosine similarity
                let dot: f32 = rows[i]
                    .2
                    .iter()
                    .zip(rows[j].2.iter())
                    .map(|(a, b)| a * b)
                    .sum();
                let sim = dot / (norms[i] * norms[j]);
                if sim >= threshold {
                    cluster.push(j);
                    assigned[j] = true;
                }
            }

            if cluster.len() >= 2 {
                clusters.push(cluster);
            }
        }

        // Convert clusters to ErrorPattern entries
        clusters
            .into_iter()
            .map(|indices| {
                let ids: Vec<i64> = indices.iter().map(|&idx| rows[idx].0).collect();
                let freq = ids.len() as u32;
                // Use the first error's message as a representative label
                let representative_msg = &rows[indices[0]].1;
                let label = if representative_msg.len() > 60 {
                    format!("{}...", &representative_msg[..60])
                } else {
                    representative_msg.clone()
                };

                ErrorPattern {
                    name: format!("Semantically similar: \"{}\"", label),
                    error_ids: ids,
                    frequency: freq,
                    pattern_type: PatternType::SimilarEmbedding,
                    suggested_cause: Some(
                        "These errors have similar semantic meaning and may share a root cause"
                            .to_string(),
                    ),
                }
            })
            .collect()
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
            workflow_name: None,
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
            trace_id: None,
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

    // ---------------------------------------------------------------
    // Embedding pattern detection tests
    // ---------------------------------------------------------------

    /// Create a minimal in-memory database with the error_events table for embedding tests.
    fn create_embedding_test_db() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE error_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                log_source_id INTEGER,
                log_source_name TEXT NOT NULL,
                task_run_id TEXT,
                workflow_step_id TEXT,
                log_timestamp TEXT,
                captured_at TEXT NOT NULL DEFAULT (datetime('now')),
                severity TEXT NOT NULL DEFAULT 'error',
                error_type TEXT,
                error_code TEXT,
                message TEXT NOT NULL,
                stack_trace TEXT,
                context_lines TEXT,
                raw_entry TEXT,
                file_path TEXT,
                line_number INTEGER,
                column_number INTEGER,
                function_name TEXT,
                signature_hash TEXT NOT NULL,
                occurrence_count INTEGER DEFAULT 1,
                first_seen_at TEXT NOT NULL DEFAULT (datetime('now')),
                last_seen_at TEXT NOT NULL DEFAULT (datetime('now')),
                status TEXT DEFAULT 'new',
                finding_id INTEGER,
                resolved_by_task_run_id TEXT,
                resolved_by_fix_id TEXT,
                resolution_notes TEXT,
                message_embedding BLOB,
                trace_id TEXT,
                acknowledged_at TEXT,
                resolved_at TEXT
            );
            "#,
        )
        .unwrap();
        conn
    }

    /// Create a 384-dim f32 embedding as a little-endian byte blob, normalized to unit length.
    fn make_embedding(base_value: f32) -> Vec<u8> {
        let dim = 384;
        let embedding: Vec<f32> = (0..dim)
            .map(|i| base_value + (i as f32) * 0.001)
            .collect();
        let norm: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
        let normalized: Vec<f32> = embedding.iter().map(|x| x / norm).collect();
        normalized
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect()
    }

    /// Insert an error event with an embedding into the test DB and return its id.
    fn insert_error_with_embedding(
        conn: &rusqlite::Connection,
        message: &str,
        embedding: &[u8],
    ) -> i64 {
        conn.execute(
            r#"INSERT INTO error_events (log_source_name, message, signature_hash, message_embedding)
               VALUES ('test', ?1, ?2, ?3)"#,
            rusqlite::params![message, format!("hash_{}", message), embedding],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    /// Helper to build an orthogonal embedding blob (non-zero only in the second half of dims).
    fn make_orthogonal_embedding() -> Vec<u8> {
        let dim = 384;
        let mut emb = vec![0.0f32; dim];
        for v in emb.iter_mut().skip(dim / 2) {
            *v = 1.0;
        }
        let norm: f32 = emb.iter().map(|x| x * x).sum::<f32>().sqrt();
        emb.iter()
            .map(|x| x / norm)
            .flat_map(|f| f.to_le_bytes())
            .collect()
    }

    #[test]
    fn test_embedding_patterns_empty_ids() {
        let conn = create_embedding_test_db();
        let curator = DebugContextCurator::new();

        let patterns = curator.detect_embedding_patterns(&conn, &[]);
        assert!(patterns.is_empty());
    }

    #[test]
    fn test_embedding_patterns_single_error() {
        let conn = create_embedding_test_db();
        let curator = DebugContextCurator::new();

        let emb = make_embedding(1.0);
        let id = insert_error_with_embedding(&conn, "single error", &emb);

        let patterns = curator.detect_embedding_patterns(&conn, &[id]);
        assert!(
            patterns.is_empty(),
            "need at least 2 errors to form a cluster"
        );
    }

    #[test]
    fn test_embedding_patterns_two_identical_embeddings() {
        let conn = create_embedding_test_db();
        let curator = DebugContextCurator::new();

        let emb = make_embedding(1.0);
        let id1 = insert_error_with_embedding(&conn, "error A", &emb);
        let id2 = insert_error_with_embedding(&conn, "error B", &emb);

        let patterns = curator.detect_embedding_patterns(&conn, &[id1, id2]);
        assert_eq!(patterns.len(), 1, "identical embeddings should cluster");
        assert_eq!(patterns[0].error_ids.len(), 2);
        assert_eq!(patterns[0].pattern_type, PatternType::SimilarEmbedding);
    }

    #[test]
    fn test_embedding_patterns_two_very_different_embeddings() {
        let conn = create_embedding_test_db();
        let curator = DebugContextCurator::new();

        // emb_a: non-zero only in the first half of dimensions
        let dim = 384;
        let mut emb_a = vec![0.0f32; dim];
        for v in emb_a.iter_mut().take(dim / 2) {
            *v = 1.0;
        }
        let norm_a: f32 = emb_a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let blob_a: Vec<u8> = emb_a
            .iter()
            .map(|x| x / norm_a)
            .flat_map(|f| f.to_le_bytes())
            .collect();

        // emb_b: non-zero only in the second half → orthogonal to emb_a
        let blob_b = make_orthogonal_embedding();

        let id1 = insert_error_with_embedding(&conn, "network error", &blob_a);
        let id2 = insert_error_with_embedding(&conn, "syntax error", &blob_b);

        let patterns = curator.detect_embedding_patterns(&conn, &[id1, id2]);
        assert!(
            patterns.is_empty(),
            "orthogonal embeddings should NOT cluster (cosine sim ~0)"
        );
    }

    #[test]
    fn test_embedding_patterns_three_errors_partial_cluster() {
        let conn = create_embedding_test_db();
        let curator = DebugContextCurator::new();

        // Two identical embeddings
        let emb_similar = make_embedding(1.0);
        let id1 = insert_error_with_embedding(&conn, "timeout error in API", &emb_similar);
        let id2 = insert_error_with_embedding(&conn, "timeout error in DB", &emb_similar);

        // One orthogonal embedding
        let blob_diff = make_orthogonal_embedding();
        let id3 = insert_error_with_embedding(&conn, "unrelated issue", &blob_diff);

        let patterns = curator.detect_embedding_patterns(&conn, &[id1, id2, id3]);
        assert_eq!(patterns.len(), 1, "should find exactly one cluster");
        assert_eq!(
            patterns[0].error_ids.len(),
            2,
            "cluster should contain only the two similar errors"
        );
        assert!(patterns[0].error_ids.contains(&id1));
        assert!(patterns[0].error_ids.contains(&id2));
        assert!(
            !patterns[0].error_ids.contains(&id3),
            "the different error should not be in the cluster"
        );
    }
}
