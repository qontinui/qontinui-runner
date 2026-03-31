//! Structured AI Output Format
//!
//! This module provides type-safe structured output for AI worker agents,
//! replacing the fragile text-marker-based parsing with proper JSON structures.
//!
//! # Design Goals
//!
//! 1. **Type Safety**: All worker outputs are validated against a schema
//! 2. **Reliability**: No regex parsing failures from malformed markers
//! 3. **Observability**: Structured data is easier to log and analyze
//! 4. **Extensibility**: Easy to add new output fields without breaking parsing
//!
//! # Usage
//!
//! Workers should output JSON in a clearly delimited block:
//!
//! ```text
//! ... natural language work description ...
//!
//! ```json:worker_output
//! {
//!   "work_summary": "Fixed 3 type errors in auth module",
//!   "signals": [{"type": "work_complete", "reason": "All issues resolved"}],
//!   "findings": [...],
//!   "files_modified": ["src/auth.ts", "src/types.ts"],
//!   "confidence": "high"
//! }
//! ```
//! ```
//!
//! # Backward Compatibility
//!
//! The parser supports both:
//! - New structured JSON format (preferred)
//! - Legacy text markers (fallback for existing prompts)

#![allow(dead_code)]

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::orchestrator::types::{Confidence, CriterionOverride, Finding, WorkerSignal};
use crate::str_utils::truncate_str;
use crate::validation::coercion::{apply_coercions, CoercionPolicy};
use crate::validation::validator::SchemaValidator;

// ============================================================================
// Structured Worker Output
// ============================================================================

/// Structured output from an AI worker agent.
///
/// This replaces the fragile text-marker parsing with type-safe JSON.
/// Workers emit this structure to communicate:
/// - What work was done
/// - Signals for orchestrator control flow
/// - Findings discovered during work
/// - Files that were modified
/// - Confidence in the work quality
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WorkerOutput {
    /// Summary of work performed in this iteration.
    /// Should be concise (1-3 sentences).
    pub work_summary: String,

    /// Signals for orchestrator control flow.
    /// Empty means "continue working".
    #[serde(default)]
    pub signals: Vec<StructuredSignal>,

    /// Findings discovered during work.
    #[serde(default)]
    pub findings: Vec<StructuredFinding>,

    /// Files that were modified in this iteration.
    #[serde(default)]
    pub files_modified: Vec<String>,

    /// Criterion overrides with justifications.
    #[serde(default)]
    pub criterion_overrides: Vec<StructuredOverride>,

    /// Confidence level in the work quality.
    #[serde(default = "default_confidence")]
    pub confidence: ConfidenceLevel,

    /// Optional suggestion for next action if work continues.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_action_suggestion: Option<String>,

    /// Optional progress estimate (0.0 to 1.0).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress_estimate: Option<f32>,

    /// Optional notes for debugging or context.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

fn default_confidence() -> ConfidenceLevel {
    ConfidenceLevel::Medium
}

impl Default for WorkerOutput {
    fn default() -> Self {
        Self {
            work_summary: String::new(),
            signals: Vec::new(),
            findings: Vec::new(),
            files_modified: Vec::new(),
            criterion_overrides: Vec::new(),
            confidence: ConfidenceLevel::Medium,
            next_action_suggestion: None,
            progress_estimate: None,
            notes: None,
        }
    }
}

impl WorkerOutput {
    /// Create a new worker output with just a summary.
    pub fn new(work_summary: impl Into<String>) -> Self {
        Self {
            work_summary: work_summary.into(),
            ..Default::default()
        }
    }

    /// Create output indicating work is complete.
    pub fn work_complete(work_summary: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            work_summary: work_summary.into(),
            signals: vec![StructuredSignal::WorkComplete {
                reason: Some(reason.into()),
            }],
            confidence: ConfidenceLevel::High,
            ..Default::default()
        }
    }

    /// Create output indicating replanning is needed.
    pub fn need_replan(work_summary: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            work_summary: work_summary.into(),
            signals: vec![StructuredSignal::NeedReplan {
                reason: reason.into(),
            }],
            ..Default::default()
        }
    }

    /// Add a finding to the output.
    pub fn with_finding(mut self, finding: StructuredFinding) -> Self {
        self.findings.push(finding);
        self
    }

    /// Add a modified file to the output.
    pub fn with_file(mut self, path: impl Into<String>) -> Self {
        self.files_modified.push(path.into());
        self
    }

    /// Add a criterion override.
    pub fn with_override(mut self, override_: StructuredOverride) -> Self {
        self.criterion_overrides.push(override_);
        self
    }

    /// Set the confidence level.
    pub fn with_confidence(mut self, confidence: ConfidenceLevel) -> Self {
        self.confidence = confidence;
        self
    }

    /// Check if this output signals work completion.
    pub fn is_work_complete(&self) -> bool {
        self.signals
            .iter()
            .any(|s| matches!(s, StructuredSignal::WorkComplete { .. }))
    }

    /// Check if this output signals replanning is needed.
    pub fn needs_replan(&self) -> bool {
        self.signals
            .iter()
            .any(|s| matches!(s, StructuredSignal::NeedReplan { .. }))
    }

    /// Convert to the legacy WorkerSignal type.
    pub fn to_worker_signal(&self) -> Option<WorkerSignal> {
        for signal in &self.signals {
            match signal {
                StructuredSignal::WorkComplete { reason } => {
                    return Some(WorkerSignal::WorkComplete {
                        reason: reason.clone(),
                    });
                }
                StructuredSignal::NeedReplan { reason } => {
                    return Some(WorkerSignal::NeedReplan {
                        reason: reason.clone(),
                    });
                }
                StructuredSignal::Continue => {}
                // RequestContext and RecordStore are handled by BrainActorOrchestrator,
                // not the legacy signal conversion path.
                StructuredSignal::RequestContext { .. }
                | StructuredSignal::RecordStore { .. } => {}
            }
        }
        None
    }

    /// Convert findings to the legacy Finding type.
    pub fn to_findings(&self) -> Vec<Finding> {
        self.findings.iter().map(|f| f.to_finding()).collect()
    }

    /// Convert overrides to the legacy CriterionOverride type.
    pub fn to_overrides(&self, iteration: u32) -> Vec<CriterionOverride> {
        self.criterion_overrides
            .iter()
            .map(|o| o.to_criterion_override(iteration))
            .collect()
    }
}

// ============================================================================
// Signal Types
// ============================================================================

/// Structured signal for orchestrator control flow.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StructuredSignal {
    /// Worker believes work is complete, ready for verification.
    WorkComplete {
        /// Optional reason explaining why work is complete.
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },

    /// Worker needs the plan to be revised.
    NeedReplan {
        /// Reason explaining why replanning is needed.
        reason: String,
    },

    /// Worker is continuing work (default, usually not emitted explicitly).
    Continue,

    /// Actor requests the Brain to re-analyze the current screen state.
    ///
    /// Emitted when the Actor encounters a situation where the screen has changed
    /// significantly, the action plan seems wrong, or more context is needed.
    /// The orchestrator will re-invoke the Brain with a fresh screenshot before
    /// the next Actor iteration.
    RequestContext {
        /// Reason explaining why re-analysis is needed.
        reason: String,
    },

    /// Worker wants to persist a key-value record for later retrieval.
    ///
    /// Used by both Brain and Actor to save intermediate findings, observations,
    /// or artifacts that may be needed in future iterations. Records are stored
    /// in the BrainActorOrchestrator's record store and included in Brain context.
    RecordStore {
        /// Key to store the value under.
        key: String,
        /// Value to store.
        value: String,
    },
}

// ============================================================================
// Finding Types
// ============================================================================

/// Structured finding from worker analysis.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StructuredFinding {
    /// Type of finding.
    #[serde(rename = "type")]
    pub finding_type: FindingType,

    /// Description of the finding.
    pub description: String,

    /// Supporting evidence (file paths, log excerpts, etc.).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,

    /// Confidence level.
    #[serde(default = "default_confidence")]
    pub confidence: ConfidenceLevel,

    /// Related file paths.
    #[serde(default)]
    pub related_files: Vec<String>,
}

impl StructuredFinding {
    /// Create a new finding.
    pub fn new(finding_type: FindingType, description: impl Into<String>) -> Self {
        Self {
            finding_type,
            description: description.into(),
            evidence: None,
            confidence: ConfidenceLevel::Medium,
            related_files: Vec::new(),
        }
    }

    /// Create a bug finding.
    pub fn bug(description: impl Into<String>) -> Self {
        Self::new(FindingType::Bug, description)
    }

    /// Create a root cause finding.
    pub fn root_cause(description: impl Into<String>) -> Self {
        Self::new(FindingType::RootCause, description)
    }

    /// Create an observation finding.
    pub fn observation(description: impl Into<String>) -> Self {
        Self::new(FindingType::Observation, description)
    }

    /// Create a solution finding.
    pub fn solution(description: impl Into<String>) -> Self {
        Self::new(FindingType::Solution, description)
    }

    /// Add evidence to the finding.
    pub fn with_evidence(mut self, evidence: impl Into<String>) -> Self {
        self.evidence = Some(evidence.into());
        self
    }

    /// Add a related file.
    pub fn with_file(mut self, path: impl Into<String>) -> Self {
        self.related_files.push(path.into());
        self
    }

    /// Convert to the legacy Finding type.
    pub fn to_finding(&self) -> Finding {
        Finding {
            id: uuid::Uuid::new_v4().to_string(),
            finding_type: self.finding_type.as_str().to_string(),
            description: self.description.clone(),
            evidence: self.evidence.clone(),
            confidence: self.confidence.to_confidence(),
            related_files: self.related_files.clone(),
        }
    }
}

/// Types of findings that workers can report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FindingType {
    /// A bug or defect in the code.
    Bug,
    /// The root cause of an issue.
    RootCause,
    /// A general observation about the codebase.
    Observation,
    /// A hypothesis about the problem.
    Hypothesis,
    /// A solution or fix that was implemented.
    Solution,
}

impl FindingType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Bug => "bug",
            Self::RootCause => "root_cause",
            Self::Observation => "observation",
            Self::Hypothesis => "hypothesis",
            Self::Solution => "solution",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "bug" | "finding" => Some(Self::Bug),
            "root_cause" | "rootcause" | "root-cause" => Some(Self::RootCause),
            "observation" => Some(Self::Observation),
            "hypothesis" => Some(Self::Hypothesis),
            "solution" | "fix" => Some(Self::Solution),
            _ => None,
        }
    }
}

// ============================================================================
// Override Types
// ============================================================================

/// Structured criterion override.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StructuredOverride {
    /// The criterion ID being overridden.
    pub criterion_id: String,

    /// What specifically is being overridden (e.g., class name, file path).
    pub item: String,

    /// Justification for why this override is acceptable.
    pub justification: String,
}

impl StructuredOverride {
    /// Create a new override.
    pub fn new(
        criterion_id: impl Into<String>,
        item: impl Into<String>,
        justification: impl Into<String>,
    ) -> Self {
        Self {
            criterion_id: criterion_id.into(),
            item: item.into(),
            justification: justification.into(),
        }
    }

    /// Convert to the legacy CriterionOverride type.
    pub fn to_criterion_override(&self, iteration: u32) -> CriterionOverride {
        CriterionOverride::new(
            &self.criterion_id,
            &self.item,
            &self.justification,
            iteration,
        )
    }
}

// ============================================================================
// Confidence Level
// ============================================================================

/// Confidence level for findings and work quality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ConfidenceLevel {
    High,
    #[default]
    Medium,
    Low,
}

impl ConfidenceLevel {
    pub fn to_confidence(self) -> Confidence {
        match self {
            Self::High => Confidence::High,
            Self::Medium => Confidence::Medium,
            Self::Low => Confidence::Low,
        }
    }
}

// ============================================================================
// Parser
// ============================================================================

/// Result of parsing worker output.
#[derive(Debug)]
pub struct ParsedWorkerOutput {
    /// The structured output (if found).
    pub structured: Option<WorkerOutput>,

    /// The raw text output (always present).
    pub raw_text: String,

    /// Whether the structured output was found in the new JSON format.
    pub used_json_format: bool,

    /// Any parse errors encountered.
    pub parse_errors: Vec<String>,

    /// Schema validation errors (if validation was attempted).
    pub validation_errors: Vec<String>,

    /// Whether type coercions were applied to make the output valid.
    pub coercions_applied: bool,
}

impl ParsedWorkerOutput {
    /// Get the worker output, falling back to legacy parsing if needed.
    pub fn get_output(&self, iteration: u32) -> WorkerOutput {
        if let Some(structured) = &self.structured {
            structured.clone()
        } else {
            // Fall back to legacy parsing
            parse_legacy_output(&self.raw_text, iteration)
        }
    }
}

/// Parse worker output, supporting both JSON and legacy formats.
///
/// # JSON Format (Preferred)
///
/// Workers can emit structured output in a JSON block:
///
/// ```text
/// ```json:worker_output
/// { ... WorkerOutput fields ... }
/// ```
/// ```
///
/// # Legacy Format (Fallback)
///
/// For backward compatibility, the parser also supports text markers:
/// - `[WORK_COMPLETE]` - signals work completion
/// - `[NEED_REPLAN]` - signals replanning needed
/// - `[FINDING:type]` - records a finding
/// - `[CRITERION_OVERRIDE:id]` - records an override
pub fn parse_worker_output(output: &str) -> ParsedWorkerOutput {
    let mut result = ParsedWorkerOutput {
        structured: None,
        raw_text: output.to_string(),
        used_json_format: false,
        parse_errors: Vec::new(),
        validation_errors: Vec::new(),
        coercions_applied: false,
    };

    // Try to find JSON block first
    let json_output = match extract_json_block(output) {
        Some(json) => json,
        None => return result,
    };

    // Step 1: Parse raw JSON
    let raw_value: serde_json::Value = match serde_json::from_str(&json_output) {
        Ok(v) => v,
        Err(e) => {
            warn!("Failed to parse worker output JSON: {}", e);
            result.parse_errors.push(format!("JSON parse error: {}", e));
            return result;
        }
    };

    // Step 2: Schema validation + coercion
    let (validated_value, coerced) = match SchemaValidator::for_type::<WorkerOutput>() {
        Ok(validator) => {
            // Try validation on raw value first
            let raw_result = validator.validate(&raw_value);
            if raw_result.valid {
                (raw_value, false)
            } else {
                // Apply lenient coercion and re-validate
                let schema = crate::schema_registry::schema_as_json::<WorkerOutput>();
                let (coerced_value, coercion_records) =
                    apply_coercions(&raw_value, &schema, CoercionPolicy::Lenient);

                if !coercion_records.is_empty() {
                    debug!(
                        "Applied {} coercions to WorkerOutput",
                        coercion_records.len()
                    );
                    let coerced_result = validator.validate(&coerced_value);
                    if coerced_result.valid {
                        (coerced_value, true)
                    } else {
                        // Still invalid after coercion — record errors
                        result.validation_errors = coerced_result
                            .errors
                            .iter()
                            .map(|e| e.to_string())
                            .collect();
                        (coerced_value, true)
                    }
                } else {
                    // No coercions applicable — record original errors
                    result.validation_errors =
                        raw_result.errors.iter().map(|e| e.to_string()).collect();
                    (raw_value, false)
                }
            }
        }
        Err(e) => {
            // Schema compilation failed — fall through to serde parsing
            warn!("Failed to compile WorkerOutput schema: {}", e);
            (raw_value, false)
        }
    };

    result.coercions_applied = coerced;

    // Step 3: Deserialize (with or without validation having passed)
    match serde_json::from_value::<WorkerOutput>(validated_value) {
        Ok(parsed) => {
            debug!("Successfully parsed structured worker output");
            result.structured = Some(parsed);
            result.used_json_format = true;
        }
        Err(e) => {
            warn!("Failed to deserialize worker output: {}", e);
            result
                .parse_errors
                .push(format!("Deserialization error: {}", e));
        }
    }

    result
}

/// Extract JSON block from output text.
///
/// Looks for:
/// - ```json:worker_output ... ```
/// - ```json\n{"work_summary":...} ... ```
fn extract_json_block(output: &str) -> Option<String> {
    // Try labeled block first: ```json:worker_output
    if let Some(start) = output.find("```json:worker_output") {
        let content_start = start + "```json:worker_output".len();
        if let Some(end) = output[content_start..].find("```") {
            let json = output[content_start..content_start + end].trim();
            if !json.is_empty() {
                return Some(json.to_string());
            }
        }
    }

    // Try generic json block with worker_output structure
    if let Some(start) = output.find("```json") {
        let content_start = start + "```json".len();
        // Skip optional newline
        let content_start = if output[content_start..].starts_with('\n') {
            content_start + 1
        } else {
            content_start
        };

        if let Some(end) = output[content_start..].find("```") {
            let json = output[content_start..content_start + end].trim();
            // Check if it looks like a WorkerOutput (has work_summary field)
            if json.contains("\"work_summary\"") {
                return Some(json.to_string());
            }
        }
    }

    None
}

/// Parse legacy text-marker format (fallback).
fn parse_legacy_output(output: &str, _iteration: u32) -> WorkerOutput {
    let mut result = WorkerOutput::default();

    // Parse signals
    if output.contains("[WORK_COMPLETE]") {
        let reason = extract_marker_content(output, "[WORK_COMPLETE]");
        result
            .signals
            .push(StructuredSignal::WorkComplete { reason });
    } else if output.contains("[NEED_REPLAN]") {
        let reason = extract_marker_content(output, "[NEED_REPLAN]")
            .unwrap_or_else(|| "Plan revision requested".to_string());
        result.signals.push(StructuredSignal::NeedReplan { reason });
    }

    // Parse findings
    result.findings = parse_legacy_findings(output);

    // Parse overrides
    result.criterion_overrides = parse_legacy_overrides(output);

    // Generate work summary from output (first non-empty line or truncated content)
    result.work_summary = generate_summary_from_output(output);

    result
}

/// Extract content after a marker.
fn extract_marker_content(output: &str, marker: &str) -> Option<String> {
    if let Some(start) = output.find(marker) {
        let after = &output[start + marker.len()..];
        let content = after.lines().next().unwrap_or("").trim();
        if !content.is_empty() && !content.starts_with('[') {
            return Some(content.to_string());
        }
    }
    None
}

/// Parse findings from legacy format.
fn parse_legacy_findings(output: &str) -> Vec<StructuredFinding> {
    let mut findings = Vec::new();
    let mut search_pos = 0;

    while let Some(start) = output[search_pos..].find("[FINDING:") {
        let abs_start = search_pos + start;

        if let Some(end_bracket) = output[abs_start..].find(']') {
            let abs_end = abs_start + end_bracket;
            let finding_type_str = &output[abs_start + 9..abs_end];

            let after_marker = &output[abs_end + 1..];
            let description = extract_finding_description(after_marker);

            if !description.is_empty() {
                let finding_type =
                    FindingType::from_str(finding_type_str).unwrap_or(FindingType::Observation);

                findings.push(StructuredFinding {
                    finding_type,
                    description,
                    evidence: None,
                    confidence: ConfidenceLevel::Medium,
                    related_files: Vec::new(),
                });
            }

            search_pos = abs_end + 1;
        } else {
            break;
        }
    }

    findings
}

/// Extract description text after a finding marker.
fn extract_finding_description(text: &str) -> String {
    let trimmed = text.trim_start();

    let end = trimmed
        .find('[')
        .unwrap_or(trimmed.len())
        .min(trimmed.find("\n\n").unwrap_or(trimmed.len()))
        .min(500);

    trimmed[..end].trim().to_string()
}

/// Parse overrides from legacy format.
fn parse_legacy_overrides(output: &str) -> Vec<StructuredOverride> {
    let mut overrides = Vec::new();
    let mut remaining = output;

    while let Some(start) = remaining.find("[CRITERION_OVERRIDE:") {
        let after_start = &remaining[start + 20..];
        if let Some(id_end) = after_start.find(']') {
            let criterion_id = after_start[..id_end].trim();

            let content_start = start + 20 + id_end + 1;
            if let Some(end) = remaining[content_start..].find("[/CRITERION_OVERRIDE]") {
                let content = &remaining[content_start..content_start + end];

                let item = extract_field(content, "Item")
                    .or_else(|| extract_field(content, "Class"))
                    .or_else(|| extract_field(content, "File"))
                    .unwrap_or_else(|| "unspecified".to_string());

                let justification = extract_field(content, "Justification")
                    .or_else(|| extract_field(content, "Reason"))
                    .unwrap_or_else(|| content.trim().to_string());

                overrides.push(StructuredOverride {
                    criterion_id: criterion_id.to_string(),
                    item,
                    justification,
                });

                remaining = &remaining[content_start + end + 21..];
            } else {
                remaining = &remaining[start + 20..];
            }
        } else {
            remaining = &remaining[start + 20..];
        }
    }

    overrides
}

/// Extract a field value from content (e.g., "Item: value").
fn extract_field(content: &str, field_name: &str) -> Option<String> {
    let pattern = format!("{}:", field_name);
    if let Some(start) = content.find(&pattern) {
        let after = &content[start + pattern.len()..];
        let value = after.lines().next().unwrap_or("").trim();
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

/// Generate a summary from raw output text.
fn generate_summary_from_output(output: &str) -> String {
    // Remove markers and extract meaningful content
    let cleaned: String = output
        .lines()
        .filter(|line| {
            !line.trim().starts_with('[')
                && !line.trim().starts_with("```")
                && !line.trim().is_empty()
        })
        .take(3)
        .collect::<Vec<_>>()
        .join(" ");

    if cleaned.len() > 200 {
        format!("{}...", truncate_str(&cleaned, 197))
    } else if cleaned.is_empty() {
        "Work performed (no summary provided)".to_string()
    } else {
        cleaned
    }
}

// ============================================================================
// Prompt Template
// ============================================================================

/// Generate the output format instructions for worker prompts.
///
/// Include this in worker system prompts to instruct them on the
/// expected output format.
///
/// Delegates to `schema_registry::prompt_instructions_for::<WorkerOutput>()`
/// for schema-derived field documentation, then appends a hand-written
/// example block that the schema generator does not produce.
pub fn worker_output_instructions() -> String {
    let mut instructions = crate::schema_registry::prompt_instructions_for::<WorkerOutput>();

    // Append hand-written guidance and example that the schema generator
    // does not produce — these give the AI concrete signal semantics and
    // a full example to imitate.
    instructions.push_str(r#"
### Signals

Use signals to communicate with the orchestrator:

- **Work complete**: `{"type": "work_complete", "reason": "Why work is done"}`
- **Need replan**: `{"type": "need_replan", "reason": "Why plan needs revision"}`

If neither applies, leave `signals` empty to continue working.

### Example Complete Output

```json:worker_output
{
  "work_summary": "Fixed 3 type errors in the auth module",
  "signals": [{"type": "work_complete", "reason": "All type errors resolved"}],
  "findings": [
    {
      "type": "root_cause",
      "description": "Missing null check in validateToken",
      "related_files": ["src/auth/validate.ts"]
    }
  ],
  "files_modified": ["src/auth/validate.ts", "src/auth/types.ts"],
  "confidence": "high",
  "progress_estimate": 1.0
}
```
"#);

    instructions
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_json_format() {
        let output = r#"
I analyzed the code and fixed the issues.

```json:worker_output
{
  "work_summary": "Fixed type errors in auth module",
  "signals": [{"type": "work_complete", "reason": "All issues resolved"}],
  "findings": [
    {
      "type": "bug",
      "description": "Missing null check",
      "confidence": "high"
    }
  ],
  "files_modified": ["src/auth.ts"],
  "confidence": "high"
}
```
"#;

        let parsed = parse_worker_output(output);
        assert!(parsed.used_json_format);
        assert!(parsed.structured.is_some());

        let output = parsed.structured.unwrap();
        assert_eq!(output.work_summary, "Fixed type errors in auth module");
        assert!(output.is_work_complete());
        assert_eq!(output.findings.len(), 1);
        assert_eq!(output.files_modified.len(), 1);
    }

    #[test]
    fn test_parse_legacy_format() {
        let output = r#"
I've fixed the issue.

[FINDING:solution] Updated the regex to include + character

[WORK_COMPLETE] Validation now works correctly
"#;

        let parsed = parse_worker_output(output);
        assert!(!parsed.used_json_format);
        assert!(parsed.structured.is_none());

        let output = parsed.get_output(1);
        assert!(output.is_work_complete());
        assert_eq!(output.findings.len(), 1);
        assert_eq!(output.findings[0].finding_type, FindingType::Solution);
    }

    #[test]
    fn test_parse_legacy_override() {
        let output = r#"
[CRITERION_OVERRIDE:god_class_detection]
Item: ConfigManager
Justification: Facade pattern delegates to specialized subsystems
[/CRITERION_OVERRIDE]
"#;

        let parsed = parse_worker_output(output);
        let output = parsed.get_output(1);
        assert_eq!(output.criterion_overrides.len(), 1);
        assert_eq!(
            output.criterion_overrides[0].criterion_id,
            "god_class_detection"
        );
        assert_eq!(output.criterion_overrides[0].item, "ConfigManager");
    }

    #[test]
    fn test_worker_output_builder() {
        let output = WorkerOutput::work_complete("Fixed all issues", "Tests pass")
            .with_finding(StructuredFinding::solution("Added null check"))
            .with_file("src/main.ts")
            .with_confidence(ConfidenceLevel::High);

        assert!(output.is_work_complete());
        assert_eq!(output.findings.len(), 1);
        assert_eq!(output.files_modified.len(), 1);
        assert_eq!(output.confidence, ConfidenceLevel::High);
    }

    #[test]
    fn test_conversion_to_legacy_types() {
        let output = WorkerOutput {
            work_summary: "Test".into(),
            signals: vec![StructuredSignal::WorkComplete {
                reason: Some("Done".into()),
            }],
            findings: vec![StructuredFinding::bug("Test bug")],
            criterion_overrides: vec![StructuredOverride::new("crit1", "item1", "reason1")],
            ..Default::default()
        };

        let signal = output.to_worker_signal();
        assert!(matches!(signal, Some(WorkerSignal::WorkComplete { .. })));

        let findings = output.to_findings();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].finding_type, "bug");

        let overrides = output.to_overrides(5);
        assert_eq!(overrides.len(), 1);
        assert_eq!(overrides[0].iteration, 5);
    }
}
