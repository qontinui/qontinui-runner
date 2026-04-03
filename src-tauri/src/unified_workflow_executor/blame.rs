//! Blame attribution engine for the verification-agentic loop.
//!
//! When a verification step fails, this module determines *which iteration's
//! code changes* likely caused the failure by cross-referencing error output
//! (file paths, error messages) against the accumulated per-iteration diffs.
//!
//! The blame engine is deterministic — no LLM calls. It uses:
//! 1. File path extraction from stdout/stderr via regex
//! 2. Cross-referencing against `accumulated_diffs` to find which iteration
//!    last modified each implicated file
//! 3. Confidence scoring based on match quality
//!
//! ## Integration
//!
//! Called from `handle_build_failure_context()` in `loop_handlers.rs`.
//! The blame context is injected into the AI's failure prompt so it knows
//! exactly which previous change caused the current failure.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;
use tracing::{debug, info};

use super::types::IterationDiff;
use crate::step_executor::{StepExecutionResult, VerificationPhaseResult};

// ---------------------------------------------------------------------------
// Static regex patterns (compiled once)
// ---------------------------------------------------------------------------

/// Generic file path pattern: recognized directory prefixes + known extensions.
static RE_FILE_PATH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?:^|[\s'"(,>])((?:\./)?(?:src|lib|app|pages|components|modules|packages|crates|tests?|spec|config|scripts|migrations|prisma|build|internal|cmd|pkg)/[\w./\-]+\.(?:tsx|jsx|ts|js|rs|py|go|java|scss|css|html|vue|svelte|json|toml|yaml|yml))"#
    ).unwrap()
});

/// Rust compiler style: `--> path/to/file.rs:10:5`
static RE_RUST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"-->\s+([\w./\-]+\.(?:rs|toml)):(\d+)"#).unwrap());

/// TypeScript compiler style: `path/to/file.ts(10,5)`
static RE_TS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"([\w./\-]+\.(?:tsx|jsx|ts|js))\((\d+),(\d+)\)"#).unwrap());

/// Python traceback style: `File "path/to/file.py", line 10`
static RE_PY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"File\s+"([\w./\-]+\.py)""#).unwrap());

/// Webpack/Vite style: `ERROR in ./src/App.tsx`
static RE_WEBPACK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:ERROR|error)\s+in\s+(\.?/[\w./\-]+\.(?:tsx|jsx|ts|js|vue|svelte))"#).unwrap()
});

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Attribution of a verification failure to a specific iteration's changes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlameAttribution {
    /// Name of the verification step that failed.
    pub failed_step: String,
    /// Which iteration introduced the change that likely caused this failure.
    pub blamed_iteration: u32,
    /// The specific file(s) and change(s) implicated.
    pub implicated_changes: Vec<ImplicatedChange>,
    /// Confidence: how certain we are this attribution is correct (0.0-1.0).
    pub confidence: f64,
    /// Human-readable explanation of the attribution.
    pub explanation: String,
}

/// A specific file change implicated in a verification failure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImplicatedChange {
    /// Path of the file implicated.
    pub file_path: String,
    /// Iteration that last modified this file.
    pub iteration_introduced: u32,
    /// Human-readable summary of the change.
    pub change_summary: String,
    /// Line range changed in the file (start, end), if available from diff hunk headers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines_changed: Option<(usize, usize)>,
}

/// Result of running blame analysis across all failed verification steps.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlameReport {
    /// Per-step blame attributions.
    pub attributions: Vec<BlameAttribution>,
    /// Files that have been blamed in 3+ consecutive iterations (oscillation signal).
    pub oscillating_files: Vec<OscillatingFile>,
    /// Files exhibiting a revert pattern (changed → reverted → changed again).
    pub revert_patterns: Vec<RevertPattern>,
}

/// A file that has been blamed across multiple consecutive iterations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OscillatingFile {
    /// The file path.
    pub file_path: String,
    /// How many consecutive iterations this file has been blamed.
    pub consecutive_blames: u32,
    /// The iterations in which this file was blamed.
    pub blamed_iterations: Vec<u32>,
}

/// A detected revert pattern: iteration N changes file, N+1 reverts, N+2 changes again.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevertPattern {
    /// The file exhibiting the pattern.
    pub file_path: String,
    /// The iterations involved in the pattern.
    pub iterations: Vec<u32>,
}

// ---------------------------------------------------------------------------
// Blame Engine
// ---------------------------------------------------------------------------

/// Analyze verification failures and attribute them to specific iterations.
///
/// # Arguments
/// * `verification_result` - The current iteration's verification result
/// * `accumulated_diffs` - All diffs from previous iterations
/// * `iteration_results` - Previous iteration results (for oscillation detection)
/// * `current_iteration` - The current iteration number
pub fn analyze_blame(
    verification_result: &VerificationPhaseResult,
    accumulated_diffs: &[IterationDiff],
    current_iteration: u32,
) -> BlameReport {
    if accumulated_diffs.is_empty() {
        return BlameReport {
            attributions: vec![],
            oscillating_files: vec![],
            revert_patterns: vec![],
        };
    }

    let mut attributions = Vec::new();

    // For each failed step, try to attribute blame
    for step_result in &verification_result.step_results {
        if step_result.success {
            continue;
        }

        if let Some(attribution) =
            attribute_step_failure(step_result, accumulated_diffs, current_iteration)
        {
            attributions.push(attribution);
        }
    }

    // Detect oscillating files (blamed in 3+ consecutive iterations)
    let oscillating_files = detect_oscillating_files(accumulated_diffs);

    // Detect revert patterns (excluding files already flagged as oscillating)
    let revert_patterns = detect_revert_patterns(accumulated_diffs, &oscillating_files);

    if !attributions.is_empty() {
        info!(
            "BLAME: Attributed {} failure(s) to specific iterations (iteration {})",
            attributions.len(),
            current_iteration
        );
    }

    BlameReport {
        attributions,
        oscillating_files,
        revert_patterns,
    }
}

/// Try to attribute a single step failure to a specific iteration.
fn attribute_step_failure(
    step_result: &StepExecutionResult,
    accumulated_diffs: &[IterationDiff],
    current_iteration: u32,
) -> Option<BlameAttribution> {
    // Extract file paths from the step's error output
    let mentioned_files = extract_file_paths(step_result);

    if mentioned_files.is_empty() {
        // No file paths found in error output — low-confidence generic blame
        // Blame the most recent iteration (it's the most likely cause)
        if let Some(last_diff) = accumulated_diffs.last() {
            return Some(BlameAttribution {
                failed_step: step_result.step_name.clone(),
                blamed_iteration: last_diff.iteration,
                implicated_changes: vec![],
                confidence: 0.3,
                explanation: format!(
                    "No specific file paths found in error output. \
                     Blaming most recent iteration {} by default.",
                    last_diff.iteration
                ),
            });
        }
        return None;
    }

    // Cross-reference mentioned files against diffs to find which iteration last modified them
    let mut implicated: Vec<ImplicatedChange> = Vec::new();
    let mut blamed_iterations: HashMap<u32, usize> = HashMap::new(); // iteration → count

    for mentioned_file in &mentioned_files {
        // Find the most recent iteration that modified this file
        if let Some((iteration, diff)) =
            find_most_recent_modifier(mentioned_file, accumulated_diffs)
        {
            let change_summary =
                format!("Modified in iteration {} ({})", iteration, diff.diff_stat);
            let lines_changed = extract_hunk_lines_for_file(mentioned_file, &diff.diff_summary);
            implicated.push(ImplicatedChange {
                file_path: mentioned_file.clone(),
                iteration_introduced: iteration,
                change_summary,
                lines_changed,
            });
            *blamed_iterations.entry(iteration).or_insert(0) += 1;
        }
    }

    if implicated.is_empty() {
        return None;
    }

    // The iteration with the most implicated files gets blamed
    let (blamed_iteration, match_count) = blamed_iterations
        .iter()
        .max_by_key(|(_, count)| *count)
        .map(|(iter, count)| (*iter, *count))
        .unwrap();

    // Confidence scoring
    let confidence = if match_count == 1 && mentioned_files.len() == 1 {
        // Single file explicitly in error output, matches a single iteration's diff
        0.9
    } else if match_count >= 2 {
        // Multiple files point to the same iteration — very strong signal
        0.95
    } else if mentioned_files.len() > implicated.len() {
        // Some mentioned files weren't found in any diff — moderate confidence
        0.6
    } else {
        0.7
    };

    let explanation = if implicated.len() == 1 {
        format!(
            "File `{}` explicitly referenced in error output was modified in iteration {}.",
            implicated[0].file_path, blamed_iteration
        )
    } else {
        let file_list: Vec<&str> = implicated
            .iter()
            .filter(|ic| ic.iteration_introduced == blamed_iteration)
            .map(|ic| ic.file_path.as_str())
            .collect();
        format!(
            "{} file(s) referenced in error output were modified in iteration {}: {}",
            file_list.len(),
            blamed_iteration,
            file_list.join(", ")
        )
    };

    Some(BlameAttribution {
        failed_step: step_result.step_name.clone(),
        blamed_iteration,
        implicated_changes: implicated,
        confidence,
        explanation,
    })
}

/// Extract file paths from a step's error output (stdout, stderr, error message).
///
/// Recognizes common error formats:
/// - TypeScript/ESLint: `src/foo.ts(10,5): error TS2305`
/// - Rust: `error[E0433]: --> src/main.rs:10:5`
/// - Generic: `src/components/Card.tsx:47:12`
/// - Python: `File "src/main.py", line 10`
/// - Webpack/Vite: `ERROR in ./src/App.tsx`
fn extract_file_paths(step_result: &StepExecutionResult) -> HashSet<String> {
    let mut paths = HashSet::new();

    // Gather all text to search through
    let mut search_text = String::new();
    if let Some(ref error) = step_result.error {
        search_text.push_str(error);
        search_text.push('\n');
    }
    if let Some(ref details) = step_result.verification_details {
        if let Some(ref stdout) = details.stdout {
            search_text.push_str(stdout);
            search_text.push('\n');
        }
        if let Some(ref stderr) = details.stderr {
            search_text.push_str(stderr);
            search_text.push('\n');
        }
    }

    if search_text.is_empty() {
        return paths;
    }

    for cap in RE_FILE_PATH.captures_iter(&search_text) {
        paths.insert(normalize_path(&cap[1]));
    }
    for cap in RE_RUST.captures_iter(&search_text) {
        paths.insert(normalize_path(&cap[1]));
    }
    for cap in RE_TS.captures_iter(&search_text) {
        paths.insert(normalize_path(&cap[1]));
    }
    for cap in RE_PY.captures_iter(&search_text) {
        paths.insert(normalize_path(&cap[1]));
    }
    for cap in RE_WEBPACK.captures_iter(&search_text) {
        paths.insert(normalize_path(&cap[1]));
    }

    debug!(
        "BLAME: Extracted {} file path(s) from step '{}' error output",
        paths.len(),
        step_result.step_name
    );

    paths
}

/// Normalize a file path for comparison (strip leading ./, normalize slashes).
fn normalize_path(path: &str) -> String {
    let p = path.trim_start_matches("./");
    p.replace('\\', "/")
}

/// Extract line range from a unified diff hunk header for a specific file.
///
/// Parses `@@ -N,M +N,M @@` headers in the diff summary. Returns the first
/// hunk's new-file line range `(start, start+count)` for the given file.
fn extract_hunk_lines_for_file(file_path: &str, diff_summary: &str) -> Option<(usize, usize)> {
    if diff_summary.is_empty() {
        return None;
    }
    let normalized = normalize_path(file_path);
    let mut in_target_file = false;

    for line in diff_summary.lines() {
        // Detect diff file header: +++ b/path/to/file
        if let Some(path) = line.strip_prefix("+++ b/") {
            in_target_file = normalize_path(path.trim()) == normalized
                || normalize_path(path.trim()).ends_with(&format!("/{}", normalized));
        } else if line.starts_with("+++ ") {
            in_target_file = false;
        }

        // Parse hunk header if we're in the target file
        if in_target_file && line.starts_with("@@ ") {
            // Format: @@ -old_start,old_count +new_start,new_count @@
            let hunk_re = Regex::new(r#"@@ -\d+(?:,\d+)? \+(\d+)(?:,(\d+))? @@"#).unwrap();
            if let Some(caps) = hunk_re.captures(line) {
                let start: usize = caps[1].parse().unwrap_or(0);
                let count: usize = caps
                    .get(2)
                    .map(|m| m.as_str().parse().unwrap_or(1))
                    .unwrap_or(1);
                return Some((start, start + count));
            }
        }
    }
    None
}

/// Find the most recent iteration that modified a given file.
fn find_most_recent_modifier<'a>(
    file_path: &str,
    accumulated_diffs: &'a [IterationDiff],
) -> Option<(u32, &'a IterationDiff)> {
    let normalized = normalize_path(file_path);

    // Search from most recent to oldest
    for diff in accumulated_diffs.iter().rev() {
        for changed_file in &diff.files_changed {
            let normalized_changed = normalize_path(changed_file);
            // Check if the file paths match:
            // - Exact match
            // - Diff path is a longer prefix (e.g., diff has "packages/app/src/foo.ts", error has "src/foo.ts")
            // We do NOT match the reverse (short diff path matching long error path) to avoid
            // false positives like "format.ts" matching "src/utils/format.ts".
            if normalized_changed == normalized
                || normalized_changed.ends_with(&format!("/{}", normalized))
            {
                return Some((diff.iteration, diff));
            }
        }
    }

    None
}

/// Detect files that appear in diffs across 3+ consecutive iterations (oscillation signal).
fn detect_oscillating_files(accumulated_diffs: &[IterationDiff]) -> Vec<OscillatingFile> {
    if accumulated_diffs.len() < 3 {
        return vec![];
    }

    // Count consecutive appearances per file (from most recent backwards)
    let mut file_streaks: HashMap<String, Vec<u32>> = HashMap::new();

    for diff in accumulated_diffs {
        for file in &diff.files_changed {
            let normalized = normalize_path(file);
            file_streaks
                .entry(normalized)
                .or_default()
                .push(diff.iteration);
        }
    }

    let mut oscillating = Vec::new();

    for (file_path, iterations) in &file_streaks {
        // Check for 3+ consecutive iterations
        if iterations.len() < 3 {
            continue;
        }

        // Find the longest consecutive streak from the end
        let mut consecutive = 1u32;
        for window in iterations.windows(2).rev() {
            if window[1] == window[0] + 1 {
                consecutive += 1;
            } else {
                break;
            }
        }

        if consecutive >= 3 {
            // Only include the consecutive tail, not all iterations
            let start = iterations.len() - consecutive as usize;
            oscillating.push(OscillatingFile {
                file_path: file_path.clone(),
                consecutive_blames: consecutive,
                blamed_iterations: iterations[start..].to_vec(),
            });
        }
    }

    oscillating.sort_by(|a, b| b.consecutive_blames.cmp(&a.consecutive_blames));
    oscillating
}

/// Detect revert patterns: a file modified, then NOT modified, then modified again.
///
/// A true alternating pattern (present, absent, present) suggests the AI is
/// changing a file, having its changes undone or overridden, then changing it again.
/// We require at least 2 gaps to avoid false positives from normal development.
///
/// Files already flagged as oscillating (3+ consecutive) are excluded — they
/// have a different (and stronger) signal.
fn detect_revert_patterns(
    accumulated_diffs: &[IterationDiff],
    oscillating_files: &[OscillatingFile],
) -> Vec<RevertPattern> {
    if accumulated_diffs.len() < 3 {
        return vec![];
    }

    // Build set of already-flagged oscillating files to exclude
    let oscillating_set: HashSet<&str> = oscillating_files
        .iter()
        .map(|o| o.file_path.as_str())
        .collect();

    // Build per-file iteration presence map
    let total_iterations: u32 = accumulated_diffs.last().map(|d| d.iteration).unwrap_or(0);
    let mut file_iterations: HashMap<String, Vec<u32>> = HashMap::new();

    for diff in accumulated_diffs {
        for file in &diff.files_changed {
            let normalized = normalize_path(file);
            file_iterations
                .entry(normalized)
                .or_default()
                .push(diff.iteration);
        }
    }

    let mut patterns = Vec::new();

    for (file_path, iterations) in &file_iterations {
        if iterations.len() < 3 || oscillating_set.contains(file_path.as_str()) {
            continue;
        }

        // Count gaps in the iteration sequence: how many times was the file
        // modified, then NOT modified, then modified again?
        let mut gap_count = 0u32;
        for window in iterations.windows(2) {
            if window[1] > window[0] + 1 {
                gap_count += 1;
            }
        }

        // Require at least 2 gaps (modified, skip, modified, skip, modified)
        // to distinguish from normal "touched once early, touched again later"
        if gap_count >= 2 {
            patterns.push(RevertPattern {
                file_path: file_path.clone(),
                iterations: iterations.clone(),
            });
        }
    }

    patterns
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

/// Format a BlameReport as a markdown section for injection into the AI's failure context.
pub fn format_blame_context(report: &BlameReport) -> String {
    if report.attributions.is_empty()
        && report.oscillating_files.is_empty()
        && report.revert_patterns.is_empty()
    {
        return String::new();
    }

    let mut output = String::from("## Failure Attribution\n\n");
    output.push_str(
        "The following verification failures appear to be caused by changes from specific iterations:\n\n",
    );

    for attr in &report.attributions {
        let confidence_label = if attr.confidence >= 0.8 {
            "High"
        } else if attr.confidence >= 0.5 {
            "Medium"
        } else {
            "Low"
        };

        output.push_str(&format!("### {} (FAILED)\n", attr.failed_step));
        output.push_str(&format!(
            "**Blamed:** Iteration {}\n",
            attr.blamed_iteration
        ));
        output.push_str(&format!("**Confidence:** {}\n", confidence_label));
        output.push_str(&format!("**Reason:** {}\n", attr.explanation));

        if !attr.implicated_changes.is_empty() {
            output.push_str("**Implicated files:**\n");
            for change in &attr.implicated_changes {
                output.push_str(&format!(
                    "- `{}` (iteration {}) — {}\n",
                    change.file_path, change.iteration_introduced, change.change_summary
                ));
            }
        }

        output.push_str(
            "**Suggestion:** Focus your fix on the implicated file(s) from the blamed iteration \
             rather than making changes elsewhere.\n\n",
        );
    }

    // Oscillation warnings
    if !report.oscillating_files.is_empty() {
        output.push_str("### STOP: File Oscillation Detected\n\n");
        output.push_str(
            "The following files have been modified in 3+ consecutive iterations, \
             indicating you are oscillating on them. **Take a fundamentally different approach:**\n\n",
        );
        for osc in &report.oscillating_files {
            output.push_str(&format!(
                "- `{}` — modified in {} consecutive iterations ({:?})\n",
                osc.file_path, osc.consecutive_blames, osc.blamed_iterations
            ));
        }
        output.push('\n');
    }

    // Revert pattern warnings
    if !report.revert_patterns.is_empty() {
        output.push_str("### WARNING: Revert Pattern Detected\n\n");
        output.push_str(
            "The following files show a change-revert-change pattern across iterations. \
             This suggests your fixes are being undone. Try a different approach entirely:\n\n",
        );
        for rp in &report.revert_patterns {
            output.push_str(&format!(
                "- `{}` — modified in iterations {:?}\n",
                rp.file_path, rp.iterations
            ));
        }
        output.push('\n');
    }

    output
}

// ---------------------------------------------------------------------------
// Escalation Policy
// ---------------------------------------------------------------------------

/// Escalation policy for stuck loops.
///
/// Controls what happens when the loop fails to make progress after N iterations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EscalationPolicy {
    /// Emit a warning event after N iterations without progress.
    pub warn_after: Option<u32>,

    /// Inject a "step back and rethink" meta-prompt after N fruitless iterations.
    /// This is the simpler alternative to full mid-run architecture switching.
    pub rethink_after: Option<u32>,

    /// Maximum wall-clock time before forced stop (seconds).
    pub time_limit_secs: Option<u64>,
}

impl Default for EscalationPolicy {
    fn default() -> Self {
        Self {
            warn_after: Some(3),
            rethink_after: Some(5),
            time_limit_secs: None,
        }
    }
}

impl EscalationPolicy {
    /// Check if a "rethink" meta-prompt should be injected at this iteration.
    ///
    /// Fires at the threshold, then every 3 iterations after to avoid noise
    /// while still reinforcing the message periodically.
    pub fn should_rethink(&self, iterations_without_progress: u32) -> bool {
        self.rethink_after
            .map(|threshold| {
                iterations_without_progress >= threshold
                    && (iterations_without_progress - threshold).is_multiple_of(3)
            })
            .unwrap_or(false)
    }

    /// Check if a warning should be emitted at this iteration.
    pub fn should_warn(&self, iterations_without_progress: u32) -> bool {
        self.warn_after
            .map(|threshold| iterations_without_progress >= threshold)
            .unwrap_or(false)
    }

    /// Check if the time limit has been exceeded.
    pub fn is_time_exceeded(&self, elapsed_secs: u64) -> bool {
        self.time_limit_secs
            .map(|limit| elapsed_secs >= limit)
            .unwrap_or(false)
    }

    /// Generate the "rethink" meta-prompt for injection into failure context.
    pub fn rethink_prompt(iterations_without_progress: u32) -> String {
        format!(
            "\n### ESCALATION: Step Back and Rethink\n\n\
             You have made **{} iterations without progress**. Your incremental approach is not working.\n\n\
             Before making any code changes:\n\
             1. **Re-read all failing verification steps** from scratch — do not rely on your memory of what they check\n\
             2. **List every assumption** you have made about the cause of the failures\n\
             3. **Challenge each assumption** — which ones have you actually verified vs. assumed?\n\
             4. **Consider a completely different approach** — if you've been editing file A, maybe the fix is in file B\n\
             5. **If the fix requires changes outside the codebase** (environment, config, external service), \
                declare it as `[UNFIXABLE_ERRORS]` with an explanation\n\n\
             Do NOT make the same type of change you've been making. Try something fundamentally different.\n",
            iterations_without_progress
        )
    }
}

// ---------------------------------------------------------------------------
// Cross-run learning integration
// ---------------------------------------------------------------------------

/// Record blame attributions as causal events for cross-run pattern detection.
///
/// Each attribution becomes a causal edge:
///   cause: "iteration_change" (iteration N modified file X)
///   effect: "verification_failure" (step Y failed)
///   relationship: "caused_by_change"
///
/// This feeds into `cross_run_learning.rs` — if the same file is blamed across
/// multiple runs, it's a systemic issue worth surfacing as a pattern.
pub fn record_blame_as_causal_events(task_run_id: &str, workflow_name: &str, report: &BlameReport) {
    for attr in &report.attributions {
        let cause_id = format!("iter-{}-change", attr.blamed_iteration);
        let effect_id = format!("verification-{}", attr.failed_step);
        let confidence = if attr.confidence >= 0.8 {
            "high"
        } else if attr.confidence >= 0.5 {
            "medium"
        } else {
            "low"
        };

        let files: Vec<&str> = attr
            .implicated_changes
            .iter()
            .map(|c| c.file_path.as_str())
            .collect();
        let description = format!(
            "Iteration {} change to [{}] caused '{}' to fail (confidence: {:.0}%)",
            attr.blamed_iteration,
            files.join(", "),
            attr.failed_step,
            attr.confidence * 100.0,
        );

        if let Err(e) = crate::reflection::causal::insert_causal_event(
            "iteration_change",
            &cause_id,
            "verification_failure",
            &effect_id,
            "caused_by_change",
            confidence,
            "blame_engine",
            Some(task_run_id),
            Some(workflow_name),
            Some(&description),
        ) {
            tracing::warn!("Failed to record blame causal event: {}", e);
        }
    }

    // Record oscillation patterns as causal events too
    for osc in &report.oscillating_files {
        let cause_id = format!("oscillation-{}", osc.file_path);
        let effect_id = format!("stuck-on-{}", osc.file_path);
        let description = format!(
            "File '{}' modified in {} consecutive iterations ({:?}) — oscillation pattern",
            osc.file_path, osc.consecutive_blames, osc.blamed_iterations,
        );

        if let Err(e) = crate::reflection::causal::insert_causal_event(
            "file_oscillation",
            &cause_id,
            "convergence_stuck",
            &effect_id,
            "oscillation_causes_stuck",
            "high",
            "blame_engine",
            Some(task_run_id),
            Some(workflow_name),
            Some(&description),
        ) {
            tracing::warn!("Failed to record oscillation causal event: {}", e);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::step_executor::{
        StepExecutionConfig, StepExecutionResult, VerificationPhaseResult, VerificationStepDetails,
    };

    fn make_step_result(
        name: &str,
        success: bool,
        stdout: Option<&str>,
        stderr: Option<&str>,
        error: Option<&str>,
    ) -> StepExecutionResult {
        StepExecutionResult {
            step_index: 0,
            step_type: "command".to_string(),
            step_name: name.to_string(),
            step_id: None,
            success,
            error: error.map(|s| s.to_string()),
            screenshot_path: None,
            started_at: None,
            ended_at: None,
            duration_ms: 100,
            config: StepExecutionConfig {
                ..Default::default()
            },
            verification_details: Some(VerificationStepDetails {
                step_id: "test".to_string(),
                phase: "verification".to_string(),
                stdout: stdout.map(|s| s.to_string()),
                stderr: stderr.map(|s| s.to_string()),
                assertions_passed: None,
                assertions_total: None,
                console_output: None,
                page_snapshot: None,
                exit_code: Some(1),
                check_results: None,
                console_errors: None,
            }),
            output_data: None,
            required: None,
            resolved_inputs: None,
            extracted_values: None,
            failure_category: None,
            interrupted: None,
        }
    }

    fn make_diff(iteration: u32, files: Vec<&str>) -> IterationDiff {
        IterationDiff {
            iteration,
            files_changed: files.into_iter().map(|s| s.to_string()).collect(),
            diff_stat: format!("{} file(s) changed", iteration),
            diff_summary: String::new(),
            insertions: 5,
            deletions: 2,
            commit_before: Some(format!("before-{}", iteration)),
            commit_after: Some(format!("after-{}", iteration)),
        }
    }

    #[test]
    fn test_extract_file_paths_typescript_error() {
        let step = make_step_result(
            "tsc",
            false,
            Some("src/components/Card.tsx(47,12): error TS2305: Module has no exported member 'ApiResponse'"),
            None,
            None,
        );
        let paths = extract_file_paths(&step);
        assert!(
            paths.contains("src/components/Card.tsx"),
            "paths: {:?}",
            paths
        );
    }

    #[test]
    fn test_extract_file_paths_rust_error() {
        let step = make_step_result(
            "cargo check",
            false,
            None,
            Some("error[E0433]: failed to resolve\n  --> src/main.rs:10:5"),
            None,
        );
        let paths = extract_file_paths(&step);
        assert!(paths.contains("src/main.rs"), "paths: {:?}", paths);
    }

    #[test]
    fn test_extract_file_paths_webpack_error() {
        let step = make_step_result(
            "build",
            false,
            Some("ERROR in ./src/App.tsx\nModule not found"),
            None,
            None,
        );
        let paths = extract_file_paths(&step);
        assert!(paths.contains("src/App.tsx"), "paths: {:?}", paths);
    }

    #[test]
    fn test_extract_file_paths_generic() {
        let step = make_step_result(
            "lint",
            false,
            Some("  src/lib/utils.ts:15:3 - error: Unexpected any"),
            None,
            None,
        );
        let paths = extract_file_paths(&step);
        assert!(paths.contains("src/lib/utils.ts"), "paths: {:?}", paths);
    }

    #[test]
    fn test_extract_file_paths_card_tsx() {
        let step = make_step_result(
            "CSS layout check",
            false,
            Some("Error in src/components/Card.tsx:47 - flex-wrap broke layout"),
            None,
            None,
        );
        let paths = extract_file_paths(&step);
        assert!(
            !paths.is_empty(),
            "no paths extracted; expected src/components/Card.tsx"
        );
        assert!(
            paths.contains("src/components/Card.tsx"),
            "paths: {:?}",
            paths
        );
    }

    #[test]
    fn test_single_file_blame_high_confidence() {
        let diffs = vec![
            make_diff(1, vec!["src/components/Card.tsx"]),
            make_diff(2, vec!["src/utils/format.ts"]),
        ];

        let step = make_step_result(
            "CSS layout check",
            false,
            Some("Error in src/components/Card.tsx:47 - flex-wrap broke layout"),
            None,
            None,
        );

        let verification = VerificationPhaseResult {
            iteration: 3,
            all_passed: false,
            total_steps: 2,
            passed_steps: 1,
            failed_steps: 1,
            skipped_steps: 0,
            total_duration_ms: 500,
            step_results: vec![step],
            critical_failure: false,
            console_errors: None,
            app_health: None,
            browser_events: None,
            network_failures: None,
        };

        let report = analyze_blame(&verification, &diffs, 3);
        assert_eq!(report.attributions.len(), 1);
        assert_eq!(report.attributions[0].blamed_iteration, 1);
        assert!(report.attributions[0].confidence >= 0.8);
    }

    #[test]
    fn test_multi_file_blame_same_iteration() {
        let diffs = vec![
            make_diff(1, vec!["src/types/index.ts", "src/components/Card.tsx"]),
            make_diff(2, vec!["src/utils/format.ts"]),
        ];

        let step = make_step_result(
            "TypeScript compilation",
            false,
            Some("src/types/index.ts:5 - error TS2305\nsrc/components/Card.tsx:10 - error TS2307"),
            None,
            None,
        );

        let verification = VerificationPhaseResult {
            iteration: 3,
            all_passed: false,
            total_steps: 1,
            passed_steps: 0,
            failed_steps: 1,
            skipped_steps: 0,
            total_duration_ms: 500,
            step_results: vec![step],
            critical_failure: false,
            console_errors: None,
            app_health: None,
            browser_events: None,
            network_failures: None,
        };

        let report = analyze_blame(&verification, &diffs, 3);
        assert_eq!(report.attributions.len(), 1);
        assert_eq!(report.attributions[0].blamed_iteration, 1);
        // Multiple files pointing to same iteration = very high confidence
        assert!(report.attributions[0].confidence >= 0.9);
    }

    #[test]
    fn test_no_diffs_returns_empty_report() {
        let verification = VerificationPhaseResult {
            iteration: 1,
            all_passed: false,
            total_steps: 1,
            passed_steps: 0,
            failed_steps: 1,
            skipped_steps: 0,
            total_duration_ms: 100,
            step_results: vec![make_step_result("test", false, Some("failed"), None, None)],
            critical_failure: false,
            console_errors: None,
            app_health: None,
            browser_events: None,
            network_failures: None,
        };

        let report = analyze_blame(&verification, &[], 1);
        assert!(report.attributions.is_empty());
    }

    #[test]
    fn test_oscillating_files_detected() {
        let diffs = vec![
            make_diff(1, vec!["src/components/Card.tsx"]),
            make_diff(2, vec!["src/components/Card.tsx"]),
            make_diff(3, vec!["src/components/Card.tsx"]),
        ];

        let oscillating = detect_oscillating_files(&diffs);
        assert_eq!(oscillating.len(), 1);
        assert_eq!(oscillating[0].file_path, "src/components/Card.tsx");
        assert_eq!(oscillating[0].consecutive_blames, 3);
    }

    #[test]
    fn test_format_blame_context_empty() {
        let report = BlameReport {
            attributions: vec![],
            oscillating_files: vec![],
            revert_patterns: vec![],
        };
        assert!(format_blame_context(&report).is_empty());
    }

    #[test]
    fn test_format_blame_context_with_attribution() {
        let report = BlameReport {
            attributions: vec![BlameAttribution {
                failed_step: "CSS check".to_string(),
                blamed_iteration: 2,
                implicated_changes: vec![ImplicatedChange {
                    file_path: "src/components/Card.tsx".to_string(),
                    iteration_introduced: 2,
                    change_summary: "Modified in iteration 2".to_string(),
                    lines_changed: None,
                }],
                confidence: 0.9,
                explanation: "File explicitly in error output".to_string(),
            }],
            oscillating_files: vec![],
            revert_patterns: vec![],
        };

        let context = format_blame_context(&report);
        assert!(context.contains("## Failure Attribution"));
        assert!(context.contains("CSS check"));
        assert!(context.contains("Iteration 2"));
        assert!(context.contains("High"));
    }

    #[test]
    fn test_escalation_policy_defaults() {
        let policy = EscalationPolicy::default();
        assert!(!policy.should_warn(2));
        assert!(policy.should_warn(3));
        assert!(!policy.should_rethink(4));
        assert!(policy.should_rethink(5)); // fires at threshold
        assert!(!policy.should_rethink(6)); // skipped (1 past threshold)
        assert!(!policy.should_rethink(7)); // skipped (2 past threshold)
        assert!(policy.should_rethink(8)); // fires again (3 past threshold)
    }

    #[test]
    fn test_escalation_time_limit() {
        let policy = EscalationPolicy {
            time_limit_secs: Some(300),
            ..Default::default()
        };
        assert!(!policy.is_time_exceeded(299));
        assert!(policy.is_time_exceeded(300));
    }

    #[test]
    fn test_normalize_path() {
        assert_eq!(normalize_path("./src/foo.ts"), "src/foo.ts");
        assert_eq!(normalize_path("src\\foo.ts"), "src/foo.ts");
        assert_eq!(normalize_path("src/foo.ts"), "src/foo.ts");
    }

    #[test]
    fn test_find_most_recent_modifier() {
        let diffs = vec![
            make_diff(1, vec!["src/foo.ts"]),
            make_diff(2, vec!["src/foo.ts", "src/bar.ts"]),
            make_diff(3, vec!["src/baz.ts"]),
        ];

        // Most recent modifier of foo.ts is iteration 2
        let result = find_most_recent_modifier("src/foo.ts", &diffs);
        assert!(result.is_some());
        assert_eq!(result.unwrap().0, 2);

        // baz.ts was only modified in iteration 3
        let result = find_most_recent_modifier("src/baz.ts", &diffs);
        assert_eq!(result.unwrap().0, 3);

        // Unknown file returns None
        let result = find_most_recent_modifier("src/unknown.ts", &diffs);
        assert!(result.is_none());
    }

    // =========================================================================
    // Integration Tests: Multi-iteration blame scenarios
    // =========================================================================

    /// Simulate a 4-iteration run where iteration 2 introduces a CSS bug
    /// that is caught by verification in iteration 3. Verify the blame engine
    /// correctly attributes the failure to iteration 2.
    #[test]
    fn test_integration_4_iteration_blame_attribution() {
        // Iteration 1: AI modifies src/utils/format.ts (passes verification)
        // Iteration 2: AI modifies src/components/Card.tsx (introduces CSS bug)
        // Iteration 3: Verification catches CSS layout failure mentioning Card.tsx
        // Expected: blame engine attributes the CSS failure to iteration 2
        let diffs = vec![
            make_diff(1, vec!["src/utils/format.ts"]),
            make_diff(2, vec!["src/components/Card.tsx"]),
            make_diff(3, vec!["src/api/client.ts"]),
        ];

        // Iteration 3 verification: CSS layout check fails, mentioning Card.tsx
        let css_step = make_step_result(
            "CSS layout check",
            false,
            Some("FAIL: Layout regression detected\n  Expected: flex-direction: row\n  Actual: flex-direction: column\n  in src/components/Card.tsx:47"),
            None,
            None,
        );

        // TypeScript compilation passes
        let tsc_step = make_step_result("TypeScript compilation", true, None, None, None);

        let verification = VerificationPhaseResult {
            iteration: 3,
            all_passed: false,
            total_steps: 2,
            passed_steps: 1,
            failed_steps: 1,
            skipped_steps: 0,
            total_duration_ms: 800,
            step_results: vec![css_step, tsc_step],
            critical_failure: false,
            console_errors: None,
            app_health: None,
            browser_events: None,
            network_failures: None,
        };

        let report = analyze_blame(&verification, &diffs, 3);

        // Should have exactly 1 attribution
        assert_eq!(
            report.attributions.len(),
            1,
            "Expected 1 attribution, got {:?}",
            report.attributions
        );

        let attr = &report.attributions[0];
        assert_eq!(attr.failed_step, "CSS layout check");
        assert_eq!(
            attr.blamed_iteration, 2,
            "Should blame iteration 2 (when Card.tsx was modified)"
        );
        assert!(
            attr.confidence >= 0.8,
            "Single file explicit match should be high confidence, got {}",
            attr.confidence
        );
        assert_eq!(attr.implicated_changes.len(), 1);
        assert_eq!(
            attr.implicated_changes[0].file_path,
            "src/components/Card.tsx"
        );
        assert_eq!(attr.implicated_changes[0].iteration_introduced, 2);

        // Format and verify the context contains actionable info
        let context = format_blame_context(&report);
        assert!(
            context.contains("Iteration 2"),
            "Context should mention blamed iteration"
        );
        assert!(
            context.contains("Card.tsx"),
            "Context should mention the file"
        );
        assert!(
            context.contains("High"),
            "Context should show high confidence"
        );
    }

    /// Simulate multiple failures from the same iteration — both a TS compilation
    /// error and a CSS error referencing files changed in iteration 1.
    #[test]
    fn test_integration_multi_failure_single_blamed_iteration() {
        let diffs = vec![
            make_diff(
                1,
                vec![
                    "src/types/index.ts",
                    "src/components/Card.tsx",
                    "src/styles/main.css",
                ],
            ),
            make_diff(2, vec!["src/utils/helpers.ts"]),
        ];

        let tsc_step = make_step_result(
            "TypeScript compilation",
            false,
            Some("src/types/index.ts:5:1 - error TS2305: Module '\"./types\"' has no exported member 'ApiResponse'\nsrc/components/Card.tsx:12:3 - error TS2307: Cannot find module './missing'"),
            None,
            None,
        );

        let css_step = make_step_result(
            "CSS check",
            false,
            Some("Error: src/styles/main.css - invalid property value at line 23"),
            None,
            None,
        );

        let verification = VerificationPhaseResult {
            iteration: 3,
            all_passed: false,
            total_steps: 2,
            passed_steps: 0,
            failed_steps: 2,
            skipped_steps: 0,
            total_duration_ms: 600,
            step_results: vec![tsc_step, css_step],
            critical_failure: false,
            console_errors: None,
            app_health: None,
            browser_events: None,
            network_failures: None,
        };

        let report = analyze_blame(&verification, &diffs, 3);

        // Both failures should be attributed to iteration 1
        assert_eq!(report.attributions.len(), 2);
        for attr in &report.attributions {
            assert_eq!(
                attr.blamed_iteration, 1,
                "Step '{}' should blame iteration 1, got {}",
                attr.failed_step, attr.blamed_iteration
            );
        }

        // TS step has 2 files from iteration 1 → very high confidence
        let ts_attr = report
            .attributions
            .iter()
            .find(|a| a.failed_step == "TypeScript compilation")
            .unwrap();
        assert!(ts_attr.confidence >= 0.9);
        assert!(ts_attr.implicated_changes.len() >= 2);
    }

    /// Test oscillation detection across iterations: the same file is modified
    /// in iterations 1, 2, 3 — indicating the AI is thrashing on it.
    #[test]
    fn test_integration_oscillation_and_blame_combined() {
        let diffs = vec![
            make_diff(1, vec!["src/components/Card.tsx", "src/utils/format.ts"]),
            make_diff(2, vec!["src/components/Card.tsx"]),
            make_diff(3, vec!["src/components/Card.tsx"]),
        ];

        let step = make_step_result(
            "CSS layout check",
            false,
            Some("Layout broken in src/components/Card.tsx:47"),
            None,
            None,
        );

        let verification = VerificationPhaseResult {
            iteration: 4,
            all_passed: false,
            total_steps: 1,
            passed_steps: 0,
            failed_steps: 1,
            skipped_steps: 0,
            total_duration_ms: 300,
            step_results: vec![step],
            critical_failure: false,
            console_errors: None,
            app_health: None,
            browser_events: None,
            network_failures: None,
        };

        let report = analyze_blame(&verification, &diffs, 4);

        // Should blame iteration 3 (most recent modifier)
        assert_eq!(report.attributions.len(), 1);
        assert_eq!(report.attributions[0].blamed_iteration, 3);

        // Should detect oscillation on Card.tsx (modified in 3 consecutive iterations)
        assert!(
            !report.oscillating_files.is_empty(),
            "Should detect oscillation"
        );
        let osc = &report.oscillating_files[0];
        assert_eq!(osc.file_path, "src/components/Card.tsx");
        assert_eq!(osc.consecutive_blames, 3);

        // The formatted context should include both blame and oscillation warnings
        let context = format_blame_context(&report);
        assert!(
            context.contains("Failure Attribution"),
            "Should have attribution section"
        );
        assert!(
            context.contains("STOP: File Oscillation"),
            "Should have oscillation warning"
        );
        assert!(
            context.contains("Card.tsx"),
            "Should mention the oscillating file"
        );
    }

    /// Test escalation policy thresholds.
    #[test]
    fn test_integration_escalation_policy_thresholds() {
        let policy = EscalationPolicy {
            warn_after: Some(3),
            rethink_after: Some(5),
            time_limit_secs: Some(600),
        };

        // Iteration 1-2: no escalation
        assert!(!policy.should_warn(1));
        assert!(!policy.should_warn(2));
        assert!(!policy.should_rethink(2));

        // Iteration 3: warn fires
        assert!(policy.should_warn(3));
        assert!(!policy.should_rethink(3));

        // Iteration 5: rethink fires
        assert!(policy.should_warn(5));
        assert!(policy.should_rethink(5));

        // Time limit
        assert!(!policy.is_time_exceeded(599));
        assert!(policy.is_time_exceeded(600));
        assert!(policy.is_time_exceeded(601));

        // The rethink prompt should contain actionable guidance
        let prompt = EscalationPolicy::rethink_prompt(5);
        assert!(prompt.contains("5 iterations without progress"));
        assert!(prompt.contains("ESCALATION"));
        assert!(prompt.contains("fundamentally different"));
    }

    /// Test that when no file paths can be extracted from error output,
    /// the blame engine falls back to blaming the most recent iteration.
    #[test]
    fn test_integration_no_file_paths_fallback_blame() {
        let diffs = vec![
            make_diff(1, vec!["src/main.rs"]),
            make_diff(2, vec!["src/lib.rs"]),
        ];

        // Error output with no recognizable file paths
        let step = make_step_result(
            "integration test",
            false,
            Some("FAIL: Expected status 200 but got 500\nServer returned Internal Server Error"),
            None,
            None,
        );

        let verification = VerificationPhaseResult {
            iteration: 3,
            all_passed: false,
            total_steps: 1,
            passed_steps: 0,
            failed_steps: 1,
            skipped_steps: 0,
            total_duration_ms: 200,
            step_results: vec![step],
            critical_failure: false,
            console_errors: None,
            app_health: None,
            browser_events: None,
            network_failures: None,
        };

        let report = analyze_blame(&verification, &diffs, 3);

        // Should still produce a blame (low confidence, most recent iteration)
        assert_eq!(report.attributions.len(), 1);
        assert_eq!(
            report.attributions[0].blamed_iteration, 2,
            "Should blame most recent iteration"
        );
        assert!(
            report.attributions[0].confidence <= 0.4,
            "No file match → low confidence"
        );
        assert!(report.attributions[0].implicated_changes.is_empty());
    }
}
