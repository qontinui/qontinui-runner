//! Spec Synthesis — Deterministic Verification Steps from Acceptance Criteria
//!
//! Bridges the gap between acceptance criteria and verification steps. Currently,
//! the builder agent implicitly maps criteria to steps via its prompt. This module
//! makes the mapping explicit and verifiable.
//!
//! No AI calls — purely deterministic mapping from criterion metadata to step JSON.
//!
//! ## Pipeline placement
//!
//! ```text
//! Specification → **Spec Synthesis** → Builder → Hardener → Validate
//! ```

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::time::Instant;
use tracing::{debug, info, warn};

use super::specification::{
    AcceptanceCriteria, AcceptanceCriterion, CriterionPriority, VerificationMethod,
};

// ============================================================================
// Types
// ============================================================================

/// Result of synthesizing verification steps from acceptance criteria.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SynthesisResult {
    /// Generated verification steps (as JSON values matching step schema).
    pub steps: Vec<Value>,
    /// Mapping from criterion ID to generated step IDs.
    pub criterion_step_map: HashMap<String, Vec<String>>,
    /// Criteria that could not be mapped to deterministic steps.
    pub unmapped_criteria: Vec<UnmappedCriterion>,
    /// Whether synthesis was successful.
    pub success: bool,
    /// Duration in ms.
    pub duration_ms: u64,
}

/// A criterion that could not be mapped to a deterministic verification step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnmappedCriterion {
    /// The criterion ID.
    pub criterion_id: String,
    /// Why it couldn't be mapped.
    pub reason: String,
    /// Suggested step type (prompt) as fallback.
    pub fallback_step: Option<Value>,
}

/// Coverage statistics for acceptance criteria vs. workflow steps.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoverageReport {
    /// Total criteria count.
    pub total_criteria: usize,
    /// Criteria covered by at least one step.
    pub covered_count: usize,
    /// Criteria not covered by any step.
    pub uncovered: Vec<String>,
    /// Coverage percentage (0.0-1.0).
    pub coverage_ratio: f32,
}

// ============================================================================
// Discovery context helpers
// ============================================================================

/// Detected test runner from discovery context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TestRunner {
    Jest,
    Vitest,
    Pytest,
    Cargo,
    Generic,
}

impl TestRunner {
    fn command_prefix(&self) -> &'static str {
        match self {
            TestRunner::Jest => "npx jest",
            TestRunner::Vitest => "npx vitest run",
            TestRunner::Pytest => "python -m pytest",
            TestRunner::Cargo => "cargo test",
            TestRunner::Generic => "npm test --",
        }
    }

    fn grep_flag(&self) -> &'static str {
        match self {
            TestRunner::Jest => "--testNamePattern",
            TestRunner::Vitest => "--testNamePattern",
            TestRunner::Pytest => "-k",
            TestRunner::Cargo => "--",
            TestRunner::Generic => "--grep",
        }
    }
}

/// Detect the test runner from discovery context by scanning for keywords.
fn detect_test_runner(discovery_context: &str) -> TestRunner {
    let ctx = discovery_context.to_lowercase();
    if ctx.contains("vitest") {
        TestRunner::Vitest
    } else if ctx.contains("jest") {
        TestRunner::Jest
    } else if ctx.contains("pytest") || ctx.contains("python") {
        TestRunner::Pytest
    } else if ctx.contains("cargo") || ctx.contains("rust") {
        TestRunner::Cargo
    } else {
        TestRunner::Generic
    }
}

/// Try to extract a working directory from the discovery context.
fn detect_working_dir(discovery_context: &str) -> Option<String> {
    // Look for common patterns like "frontend/", "src/", "packages/" in the context
    for keyword in &["working_dir", "cwd", "root_dir"] {
        if let Some(pos) = discovery_context.find(keyword) {
            let after = &discovery_context[pos..];
            // Try to extract a path-like value after the keyword
            if let Some(colon_pos) = after.find(':') {
                let value_start = colon_pos + 1;
                let rest = after[value_start..].trim_start();
                let end = rest.find(['"', '\'', ',', '\n', '}']).unwrap_or(rest.len());
                let path = rest[..end].trim().trim_matches('"').trim_matches('\'');
                if !path.is_empty() {
                    return Some(path.to_string());
                }
            }
        }
    }
    None
}

// ============================================================================
// Core synthesis
// ============================================================================

/// Synthesize verification steps from acceptance criteria.
///
/// Deterministically maps each criterion to a verification step based on its
/// `method` field. Uses `discovery_context` to refine commands (e.g., choosing
/// the correct test runner). No AI calls are made.
pub fn synthesize_verification_steps(
    criteria: &AcceptanceCriteria,
    discovery_context: &str,
) -> SynthesisResult {
    let start = Instant::now();
    let test_runner = detect_test_runner(discovery_context);
    let working_dir = detect_working_dir(discovery_context);

    let mut steps: Vec<Value> = Vec::new();
    let mut criterion_step_map: HashMap<String, Vec<String>> = HashMap::new();
    let mut unmapped_criteria: Vec<UnmappedCriterion> = Vec::new();
    let mut seen_commands: HashSet<String> = HashSet::new();

    debug!(
        "Synthesizing verification steps for {} criteria (test_runner={:?})",
        criteria.criteria.len(),
        test_runner
    );

    for criterion in &criteria.criteria {
        let step_id = format!("verify-{}", criterion.id);

        match criterion.method {
            VerificationMethod::Command => {
                let command = derive_command(criterion, &working_dir);
                let dedup_key = format!("command:{}", command);

                if seen_commands.contains(&dedup_key) {
                    debug!(
                        "Skipping duplicate command step for criterion '{}'",
                        criterion.id
                    );
                    // Still map the criterion to the existing step
                    if let Some(existing_step) = steps.iter().find(|s| {
                        s.get("command")
                            .and_then(|c| c.as_str())
                            .map(|c| c == command)
                            .unwrap_or(false)
                    }) {
                        if let Some(existing_id) = existing_step.get("id").and_then(|v| v.as_str())
                        {
                            criterion_step_map
                                .entry(criterion.id.clone())
                                .or_default()
                                .push(existing_id.to_string());
                        }
                    }
                    continue;
                }
                seen_commands.insert(dedup_key);

                let mut step = json!({
                    "id": step_id,
                    "name": format!("Verify: {}", criterion.description),
                    "type": "command",
                    "phase": "verification",
                    "command": command,
                    "expected_exit_code": 0,
                    "criterion_ids": [&criterion.id]
                });

                if let Some(ref dir) = working_dir {
                    step.as_object_mut()
                        .unwrap()
                        .insert("working_dir".to_string(), json!(dir));
                }

                criterion_step_map
                    .entry(criterion.id.clone())
                    .or_default()
                    .push(step_id.clone());
                steps.push(step);
            }

            VerificationMethod::UiBridge => {
                let step = json!({
                    "id": step_id,
                    "name": format!("Verify: {}", criterion.description),
                    "type": "ui_bridge",
                    "phase": "verification",
                    "snapshot_assert": true,
                    "criterion_ids": [&criterion.id]
                });

                criterion_step_map
                    .entry(criterion.id.clone())
                    .or_default()
                    .push(step_id.clone());
                steps.push(step);
            }

            VerificationMethod::Test => {
                let command = format!(
                    "{} {} '{}'",
                    test_runner.command_prefix(),
                    test_runner.grep_flag(),
                    criterion.description.replace('\'', "\\'")
                );
                let dedup_key = format!("command:{}", command);

                if seen_commands.contains(&dedup_key) {
                    debug!(
                        "Skipping duplicate test step for criterion '{}'",
                        criterion.id
                    );
                    continue;
                }
                seen_commands.insert(dedup_key);

                let mut step = json!({
                    "id": step_id,
                    "name": format!("Test: {}", criterion.description),
                    "type": "command",
                    "phase": "verification",
                    "command": command,
                    "expected_exit_code": 0,
                    "criterion_ids": [&criterion.id]
                });

                if let Some(ref dir) = working_dir {
                    step.as_object_mut()
                        .unwrap()
                        .insert("working_dir".to_string(), json!(dir));
                }

                criterion_step_map
                    .entry(criterion.id.clone())
                    .or_default()
                    .push(step_id.clone());
                steps.push(step);
            }

            VerificationMethod::Manual => {
                debug!(
                    "Criterion '{}' is manual, adding to unmapped with fallback",
                    criterion.id
                );
                let fallback = json!({
                    "id": step_id,
                    "name": format!("Manual check: {}", criterion.description),
                    "type": "prompt",
                    "phase": "verification",
                    "prompt": format!(
                        "Manually verify: {}. Hint: {}",
                        criterion.description, criterion.verification_hint
                    ),
                    "criterion_ids": [&criterion.id]
                });

                unmapped_criteria.push(UnmappedCriterion {
                    criterion_id: criterion.id.clone(),
                    reason: "Manual verification method cannot be automated".to_string(),
                    fallback_step: Some(fallback),
                });
            }
        }
    }

    let duration_ms = start.elapsed().as_millis() as u64;
    let success = !steps.is_empty() || criteria.criteria.is_empty();

    info!(
        "Synthesized {} verification steps from {} criteria in {}ms ({} unmapped)",
        steps.len(),
        criteria.criteria.len(),
        duration_ms,
        unmapped_criteria.len()
    );

    SynthesisResult {
        steps,
        criterion_step_map,
        unmapped_criteria,
        success,
        duration_ms,
    }
}

/// Returns `true` if the command string contains shell meta-characters that
/// could allow injection (chaining, redirection, substitution).
fn contains_shell_injection(cmd: &str) -> bool {
    // Reject commands that chain or redirect via shell metacharacters.
    // This is intentionally conservative — commands that need these operators
    // should be wrapped in a script file instead.
    let dangerous_patterns: &[&str] = &["&&", "||", ";", "|", ">", "<", "$(", "${", "`"];
    dangerous_patterns.iter().any(|p| cmd.contains(p))
}

/// Shell-escape a string for safe inclusion inside single quotes.
/// Replaces `'` with `'\''` (end quote, escaped quote, start quote).
fn shell_escape_single_quoted(s: &str) -> String {
    s.replace('\'', "'\\''")
}

/// Derive a shell command from a criterion's verification hint.
///
/// Attempts to extract a concrete command from the hint text. Falls back to
/// echoing the hint if no command can be extracted.
///
/// # Security
///
/// The backtick-extraction, curl-extraction, and `Run ` prefix paths return
/// commands derived from user-authored `verification_hint` text. These commands
/// are eventually executed in a sandboxed shell. To reduce the risk of
/// injection via crafted hints, we reject any extracted command that contains
/// shell meta-characters (`;`, `&&`, `||`, `|`, redirects, or substitutions).
/// Commands that legitimately need these operators should be placed in a script
/// file and referenced by path instead.
fn derive_command(criterion: &AcceptanceCriterion, working_dir: &Option<String>) -> String {
    let hint = &criterion.verification_hint;

    // Try to extract a backtick-delimited command from the hint
    if let Some(start) = hint.find('`') {
        if let Some(end) = hint[start + 1..].find('`') {
            let extracted = &hint[start + 1..start + 1 + end];
            if !extracted.is_empty()
                && (!extracted.contains(' ') || extracted.contains("--"))
                && !contains_shell_injection(extracted)
            {
                return extracted.to_string();
            }
            // Fall through to safer alternatives if injection detected
        }
    }

    // Look for common command patterns in the hint
    let hint_lower = hint.to_lowercase();
    if hint_lower.contains("tsc") || hint_lower.contains("typecheck") {
        return "npx tsc --noEmit".to_string();
    }
    if hint_lower.contains("eslint") || hint_lower.contains("lint") {
        return "npx eslint .".to_string();
    }
    if hint_lower.contains("prettier") || hint_lower.contains("format") {
        return "npx prettier --check .".to_string();
    }
    if hint_lower.contains("build") {
        return "npm run build".to_string();
    }
    if hint_lower.contains("curl") {
        // Extract the curl command if present
        if let Some(curl_start) = hint.find("curl") {
            let rest = &hint[curl_start..];
            let end = rest
                .find('\n')
                .or_else(|| rest.find('"'))
                .unwrap_or(rest.len());
            let extracted = rest[..end].trim().to_string();
            if !contains_shell_injection(&extracted) {
                return extracted;
            }
        }
    }

    // API check pattern — construct a curl command
    if hint_lower.contains("api") || hint_lower.contains("endpoint") || hint_lower.contains("http")
    {
        // Try to extract a URL from the hint
        if let Some(url_start) = hint.find("http") {
            let rest = &hint[url_start..];
            let end = rest
                .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == ')')
                .unwrap_or(rest.len());
            let url = &rest[..end];
            if !contains_shell_injection(url) {
                return format!("curl -sf {}", url);
            }
        }
    }

    // Fallback: use the hint as-is if it looks like a command, otherwise echo it
    let _ = working_dir; // reserved for future use
    if hint.starts_with("Run ") {
        // Strip "Run " prefix and backticks
        let cmd = hint.trim_start_matches("Run ").trim_matches('`').trim();
        if !cmd.is_empty() && !contains_shell_injection(cmd) {
            return cmd.to_string();
        }
    }

    // Last resort: echo a pass/fail check (shell-escape the id to prevent injection)
    let safe_id = shell_escape_single_quoted(&criterion.id);
    format!("echo 'TODO: verify {}' && exit 1", safe_id)
}

// ============================================================================
// Merge
// ============================================================================

/// Merge synthesized verification steps into an existing workflow.
///
/// For each synthesized step, checks if a step with matching `criterion_ids`
/// already exists in the workflow. If so, skips it (the builder already created
/// a step for that criterion). Otherwise, appends the step to
/// `verification_steps`.
pub fn merge_synthesized_steps(workflow_json: &mut Value, synthesis: &SynthesisResult) {
    let existing_criterion_ids = extract_criterion_ids_from_workflow(workflow_json);

    let verification_steps = workflow_json
        .get_mut("verification_steps")
        .and_then(|v| v.as_array_mut());

    let verification_steps = match verification_steps {
        Some(arr) => arr,
        None => {
            // Create the verification_steps array if it doesn't exist
            if let Some(obj) = workflow_json.as_object_mut() {
                obj.insert("verification_steps".to_string(), json!([]));
                obj.get_mut("verification_steps")
                    .unwrap()
                    .as_array_mut()
                    .unwrap()
            } else {
                warn!("Cannot merge synthesized steps: workflow is not a JSON object");
                return;
            }
        }
    };

    let mut merged = 0usize;
    let mut skipped = 0usize;

    for step in &synthesis.steps {
        let step_criterion_ids: HashSet<String> = step
            .get("criterion_ids")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        // Check if any of this step's criterion_ids are already covered
        let already_covered = step_criterion_ids
            .iter()
            .any(|id| existing_criterion_ids.contains(id));

        if already_covered {
            skipped += 1;
        } else {
            verification_steps.push(step.clone());
            merged += 1;
        }
    }

    info!(
        "Merged {} synthesized steps, {} already covered by builder",
        merged, skipped
    );
}

/// Extract all criterion_ids referenced by steps in a workflow.
fn extract_criterion_ids_from_workflow(workflow_json: &Value) -> HashSet<String> {
    let mut ids = HashSet::new();

    for phase_key in &[
        "setup_steps",
        "execution_steps",
        "verification_steps",
        "teardown_steps",
    ] {
        if let Some(steps) = workflow_json.get(phase_key).and_then(|v| v.as_array()) {
            for step in steps {
                // Check both "criterion_ids" (array) and "criterion_id" (single string)
                if let Some(arr) = step.get("criterion_ids").and_then(|v| v.as_array()) {
                    for id in arr {
                        if let Some(s) = id.as_str() {
                            ids.insert(s.to_string());
                        }
                    }
                }
                if let Some(s) = step.get("criterion_id").and_then(|v| v.as_str()) {
                    ids.insert(s.to_string());
                }
            }
        }
    }

    ids
}

// ============================================================================
// Coverage report
// ============================================================================

/// Generate a coverage report comparing workflow steps against acceptance criteria.
///
/// Extracts all `criterion_ids` from steps across all workflow phases and compares
/// against the full set of criteria IDs.
pub fn coverage_report(workflow_json: &Value, criteria: &AcceptanceCriteria) -> CoverageReport {
    let covered_ids = extract_criterion_ids_from_workflow(workflow_json);

    let all_ids: Vec<String> = criteria.criteria.iter().map(|c| c.id.clone()).collect();
    let total_criteria = all_ids.len();

    let uncovered: Vec<String> = all_ids
        .into_iter()
        .filter(|id| !covered_ids.contains(id))
        .collect();

    let covered_count = total_criteria - uncovered.len();
    let coverage_ratio = if total_criteria > 0 {
        covered_count as f32 / total_criteria as f32
    } else {
        1.0
    };

    if !uncovered.is_empty() {
        warn!(
            "Criteria coverage: {}/{} ({:.0}%) — uncovered: {:?}",
            covered_count,
            total_criteria,
            coverage_ratio * 100.0,
            uncovered
        );
    } else {
        info!(
            "Criteria coverage: {}/{} (100%)",
            covered_count, total_criteria
        );
    }

    CoverageReport {
        total_criteria,
        covered_count,
        uncovered,
        coverage_ratio,
    }
}

// ============================================================================
// Page Spec Update — Append acceptance criteria to matching page specs
// ============================================================================

/// Result of updating page specs with acceptance criteria.
#[derive(Debug, Clone)]
pub struct PageSpecUpdateResult {
    /// Number of spec files that were updated.
    pub specs_updated: usize,
    /// Spec file paths that were updated.
    pub updated_paths: Vec<String>,
    /// Errors encountered during update.
    pub errors: Vec<String>,
}

/// Convert an acceptance criterion's priority to a spec assertion severity.
fn priority_to_severity(priority: &CriterionPriority) -> &'static str {
    match priority {
        CriterionPriority::Critical => "critical",
        CriterionPriority::Important => "warning",
        CriterionPriority::Optional => "info",
    }
}

/// Convert an acceptance criterion's category to the closest SpecCategory.
fn criterion_category_to_spec_category(category: &str) -> &'static str {
    match category {
        "compilation" | "build" => "element-presence",
        "ui-content" | "ui" | "visual" => "element-presence",
        "behavior" | "interaction" | "workflow" => "behavior",
        "style" | "design" | "theme" => "design",
        "data-integrity" | "data" | "api" => "semantic",
        "navigation" | "routing" => "navigation",
        "accessibility" | "a11y" => "accessibility",
        "form-validation" | "validation" => "form-validation",
        _ => "semantic",
    }
}

/// Convert an acceptance criterion's method to an assertionType string.
fn method_to_assertion_type(method: &VerificationMethod) -> &'static str {
    match method {
        VerificationMethod::UiBridge => "exists",
        VerificationMethod::Command => "behavior",
        VerificationMethod::Test => "behavior",
        VerificationMethod::Manual => "semantic",
    }
}

/// Build a SpecAssertion JSON value from an acceptance criterion.
fn criterion_to_spec_assertion(criterion: &AcceptanceCriterion) -> Value {
    let spec_category = criterion_category_to_spec_category(&criterion.category);
    let assertion_type = method_to_assertion_type(&criterion.method);
    let severity = priority_to_severity(&criterion.priority);

    let mut assertion = json!({
        "id": format!("wf-{}", criterion.id),
        "description": criterion.description,
        "category": spec_category,
        "severity": severity,
        "assertionType": assertion_type,
        "source": "ai-generated",
        "reviewed": false,
        "enabled": true,
        "precondition": criterion.verification_hint,
    });

    // Build target based on method
    match criterion.method {
        VerificationMethod::UiBridge => {
            assertion["target"] = json!({
                "type": "search",
                "criteria": {
                    "textContent": criterion.description
                },
                "label": criterion.description
            });
        }
        _ => {
            assertion["target"] = json!({
                "type": "search",
                "criteria": {},
                "label": criterion.description
            });
        }
    }

    assertion
}

/// Build a SpecGroup JSON value from acceptance criteria.
fn criteria_to_spec_group(criteria: &AcceptanceCriteria) -> Value {
    let assertions: Vec<Value> = criteria
        .criteria
        .iter()
        .map(criterion_to_spec_assertion)
        .collect();

    json!({
        "id": "wf-acceptance-criteria",
        "name": "Workflow Acceptance Criteria",
        "description": criteria.goal_summary,
        "category": "semantic",
        "assertions": assertions,
        "source": "ai-generated",
        "tags": ["workflow-generated", "acceptance-criteria"]
    })
}

/// Score how well a spec file matches the workflow description.
///
/// Returns a score from 0.0 to 1.0. Higher means a better match.
fn score_spec_match(spec_json: &Value, description: &str) -> f32 {
    let description_lower = description.to_lowercase();
    let mut score: f32 = 0.0;

    // Check pageUrl match
    if let Some(page_url) = spec_json
        .get("metadata")
        .and_then(|m| m.get("pageUrl"))
        .and_then(|v| v.as_str())
    {
        let url_parts: Vec<&str> = page_url
            .split('/')
            .filter(|p| p.len() > 3) // skip short generic segments like "app", "api", "new"
            .collect();
        for part in &url_parts {
            if description_lower.contains(&part.to_lowercase()) {
                score += 0.3;
            }
        }
    }

    // Check spec description overlap
    if let Some(spec_desc) = spec_json.get("description").and_then(|v| v.as_str()) {
        let spec_words: Vec<&str> = spec_desc
            .split_whitespace()
            .filter(|w| w.len() > 4)
            .collect();
        let desc_words: Vec<&str> = description_lower
            .split_whitespace()
            .filter(|w| w.len() > 4)
            .collect();

        if !spec_words.is_empty() && !desc_words.is_empty() {
            let mut matching = 0;
            for sw in &spec_words {
                let sw_lower = sw.to_lowercase();
                for dw in &desc_words {
                    if sw_lower == **dw || sw_lower.contains(*dw) || dw.contains(sw_lower.as_str())
                    {
                        matching += 1;
                        break;
                    }
                }
            }
            let overlap = matching as f32 / spec_words.len().max(desc_words.len()) as f32;
            score += overlap * 0.4;
        }
    }

    // Check component name match
    if let Some(component) = spec_json
        .get("metadata")
        .and_then(|m| m.get("component"))
        .and_then(|v| v.as_str())
    {
        if description_lower.contains(&component.to_lowercase()) {
            score += 0.3;
        }
    }

    // Check tags overlap (capped at 0.3 to prevent tag-heavy specs from dominating)
    if let Some(tags) = spec_json
        .get("metadata")
        .and_then(|m| m.get("tags"))
        .and_then(|v| v.as_array())
    {
        let mut tag_score: f32 = 0.0;
        for tag in tags {
            if let Some(tag_str) = tag.as_str() {
                if description_lower.contains(&tag_str.to_lowercase()) {
                    tag_score += 0.15;
                }
            }
        }
        score += tag_score.min(0.3);
    }

    score.min(1.0)
}

/// Classify whether acceptance criteria should update page specs.
///
/// Returns:
/// - `SpecTarget::Page` — criteria relate to a UI page (has ui_bridge methods or ui categories)
/// - `SpecTarget::Backend` — backend/infra work with no UI association
/// - `SpecTarget::Skip` — tooling/ops prompt that should not update specs
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecTarget {
    /// Criteria relate to a UI page — match against existing page specs.
    Page,
    /// Backend/infra work — use catch-all spec if no page matches.
    Backend,
    /// Tooling/ops — do not update any specs.
    Skip,
}

/// Detect whether a prompt is a tooling/ops operation that should not update specs.
fn is_ops_prompt(description: &str) -> bool {
    let d = description.to_lowercase();
    let ops_patterns = [
        "git pull",
        "git push",
        "git commit",
        "git rebase",
        "git merge",
        "git checkout",
        "git stash",
        "git reset",
        "git cherry-pick",
        "create a pr",
        "create a pull request",
        "open a pr",
        "push the branch",
        "pull latest",
        "rebase onto",
        "npm publish",
        "cargo publish",
        "deploy to",
        "run the migration",
        "database backup",
    ];
    ops_patterns.iter().any(|p| d.contains(p))
}

/// Classify acceptance criteria to determine where they should be stored.
pub fn classify_spec_target(criteria: &AcceptanceCriteria, description: &str) -> SpecTarget {
    if is_ops_prompt(description) {
        return SpecTarget::Skip;
    }

    let has_ui_bridge = criteria
        .criteria
        .iter()
        .any(|c| c.method == VerificationMethod::UiBridge);

    let has_ui_category = criteria.criteria.iter().any(|c| {
        matches!(
            c.category.as_str(),
            "ui-content" | "ui" | "visual" | "style" | "design" | "layout" | "navigation"
        )
    });

    if has_ui_bridge || has_ui_category {
        SpecTarget::Page
    } else {
        SpecTarget::Backend
    }
}

/// Write a group to a specific spec file, replacing any existing group with the same ID.
fn write_group_to_spec_file(
    path: &std::path::Path,
    group: &Value,
    group_id: &str,
    criteria_count: usize,
) -> Result<(), String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;

    let mut spec_json: Value = serde_json::from_str(&content)
        .map_err(|e| format!("Failed to parse {}: {}", path.display(), e))?;

    // Replace existing group if present
    if let Some(groups) = spec_json.get_mut("groups").and_then(|g| g.as_array_mut()) {
        let had_group = groups.len();
        groups.retain(|g| g.get("id").and_then(|v| v.as_str()) != Some(group_id));
        if groups.len() < had_group {
            debug!(
                "Spec {:?} had existing group '{}', replacing",
                path.file_name(),
                group_id
            );
        }
    }

    // Append the new group
    if let Some(groups) = spec_json.get_mut("groups").and_then(|g| g.as_array_mut()) {
        groups.push(group.clone());
    } else {
        spec_json["groups"] = json!([group.clone()]);
    }

    // Update metadata.updatedAt
    if let Some(metadata) = spec_json
        .get_mut("metadata")
        .and_then(|m| m.as_object_mut())
    {
        metadata.insert(
            "updatedAt".to_string(),
            json!(chrono::Utc::now().to_rfc3339()),
        );
    }

    let updated_content = serde_json::to_string_pretty(&spec_json)
        .map_err(|e| format!("Failed to serialize {}: {}", path.display(), e))?;

    std::fs::write(path, updated_content)
        .map_err(|e| format!("Failed to write {}: {}", path.display(), e))?;

    info!(
        "Updated spec {:?} with {} acceptance criteria",
        path.file_name(),
        criteria_count,
    );

    Ok(())
}

/// Update page spec files by appending acceptance criteria as a new SpecGroup.
///
/// Scans spec files in the given directories, scores each against the workflow
/// description, and appends the criteria as a new group to matching specs
/// (score >= `match_threshold`).
///
/// For backend/infra criteria that match no page spec, falls back to a
/// catch-all `workflow-criteria.spec.uibridge.json` file. Tooling/ops
/// prompts are skipped entirely.
pub fn update_page_specs_from_criteria(
    criteria: &AcceptanceCriteria,
    description: &str,
    spec_dirs: &[&std::path::Path],
    match_threshold: f32,
) -> PageSpecUpdateResult {
    let start = Instant::now();
    let mut result = PageSpecUpdateResult {
        specs_updated: 0,
        updated_paths: Vec::new(),
        errors: Vec::new(),
    };

    if criteria.criteria.is_empty() {
        debug!("No acceptance criteria to append to page specs");
        return result;
    }

    // Classify the prompt to decide routing
    let target = classify_spec_target(criteria, description);
    if target == SpecTarget::Skip {
        info!("Ops/tooling prompt detected, skipping spec update");
        return result;
    }

    let new_group = criteria_to_spec_group(criteria);
    let group_id = "wf-acceptance-criteria";

    // Try to match against existing page specs
    for spec_dir in spec_dirs {
        let entries = match std::fs::read_dir(spec_dir) {
            Ok(e) => e,
            Err(e) => {
                debug!("Cannot read spec dir {:?}: {}", spec_dir, e);
                continue;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

            if !file_name.ends_with(".spec.uibridge.json") {
                continue;
            }

            // Skip the catch-all file during page matching
            if file_name == "workflow-criteria.spec.uibridge.json" {
                continue;
            }

            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(e) => {
                    result
                        .errors
                        .push(format!("Failed to read {}: {}", path.display(), e));
                    continue;
                }
            };

            let spec_json: Value = match serde_json::from_str(&content) {
                Ok(v) => v,
                Err(e) => {
                    result
                        .errors
                        .push(format!("Failed to parse {}: {}", path.display(), e));
                    continue;
                }
            };

            let score = score_spec_match(&spec_json, description);

            if score < match_threshold {
                continue;
            }

            match write_group_to_spec_file(&path, &new_group, group_id, criteria.criteria.len()) {
                Ok(()) => {
                    result.specs_updated += 1;
                    result.updated_paths.push(path.display().to_string());
                }
                Err(e) => {
                    result.errors.push(e);
                }
            }
        }
    }

    // If no page spec matched, fall back to the catch-all spec file.
    // This handles backend/infra criteria that don't map to any UI page.
    // The file workflow-criteria.spec.uibridge.json is pre-created in the repo
    // and included in the spec registry so it appears in the Specs page UI.
    if result.specs_updated == 0 {
        let mut found_catchall = false;
        for spec_dir in spec_dirs {
            let catchall_path = spec_dir.join("workflow-criteria.spec.uibridge.json");
            if catchall_path.exists() {
                match write_group_to_spec_file(
                    &catchall_path,
                    &new_group,
                    group_id,
                    criteria.criteria.len(),
                ) {
                    Ok(()) => {
                        result.specs_updated += 1;
                        result
                            .updated_paths
                            .push(catchall_path.display().to_string());
                    }
                    Err(e) => {
                        result.errors.push(e);
                    }
                }
                found_catchall = true;
                break;
            }
        }
        if !found_catchall {
            debug!("No catch-all spec file (workflow-criteria.spec.uibridge.json) found in spec directories");
        }
    }

    let duration_ms = start.elapsed().as_millis();
    info!(
        "Page spec update: {} updated, {} errors in {}ms",
        result.specs_updated,
        result.errors.len(),
        duration_ms,
    );

    result
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_generation::specification::{CriterionPriority, VerificationMethod};

    fn sample_criteria() -> AcceptanceCriteria {
        AcceptanceCriteria {
            goal_summary: "Dark mode toggle works correctly".to_string(),
            criteria: vec![
                AcceptanceCriterion {
                    id: "typecheck-passes".to_string(),
                    description: "TypeScript compilation succeeds with no errors".to_string(),
                    method: VerificationMethod::Command,
                    priority: CriterionPriority::Critical,
                    verification_hint: "Run `npx tsc --noEmit` in frontend/".to_string(),
                    category: "compilation".to_string(),
                    ..Default::default()
                },
                AcceptanceCriterion {
                    id: "toggle-renders".to_string(),
                    description: "Dark mode toggle button is visible on settings page".to_string(),
                    method: VerificationMethod::UiBridge,
                    priority: CriterionPriority::Critical,
                    verification_hint: "Assert element 'toggle-dark-mode' exists via UI Bridge"
                        .to_string(),
                    category: "ui-content".to_string(),
                    ..Default::default()
                },
                AcceptanceCriterion {
                    id: "unit-tests-pass".to_string(),
                    description: "Unit tests for dark mode pass".to_string(),
                    method: VerificationMethod::Test,
                    priority: CriterionPriority::Important,
                    verification_hint: "Run unit tests for dark mode component".to_string(),
                    category: "behavior".to_string(),
                    ..Default::default()
                },
                AcceptanceCriterion {
                    id: "manual-visual-review".to_string(),
                    description: "Colors look correct in dark mode".to_string(),
                    method: VerificationMethod::Manual,
                    priority: CriterionPriority::Optional,
                    verification_hint: "Visual inspection".to_string(),
                    category: "style".to_string(),
                    ..Default::default()
                },
            ],
            assumptions: vec![
                "Project uses TypeScript".to_string(),
                "Frontend runs on localhost:3001".to_string(),
            ],
            bugfix_context: None,
        }
    }

    #[test]
    fn test_synthesize_generates_steps_for_automatable_criteria() {
        let criteria = sample_criteria();
        let result = synthesize_verification_steps(&criteria, "jest vitest");

        assert!(result.success);
        // 3 automatable criteria: command, ui_bridge, test
        assert_eq!(result.steps.len(), 3);
        // 1 unmapped (manual)
        assert_eq!(result.unmapped_criteria.len(), 1);
        assert_eq!(
            result.unmapped_criteria[0].criterion_id,
            "manual-visual-review"
        );
    }

    #[test]
    fn test_synthesize_command_step_structure() {
        let criteria = AcceptanceCriteria {
            goal_summary: "Build succeeds".to_string(),
            criteria: vec![AcceptanceCriterion {
                id: "typecheck-passes".to_string(),
                description: "TypeScript compilation succeeds".to_string(),
                method: VerificationMethod::Command,
                priority: CriterionPriority::Critical,
                verification_hint: "Run `npx tsc --noEmit`".to_string(),
                category: "compilation".to_string(),
                ..Default::default()
            }],
            assumptions: vec![],
            bugfix_context: None,
        };

        let result = synthesize_verification_steps(&criteria, "");
        assert_eq!(result.steps.len(), 1);

        let step = &result.steps[0];
        assert_eq!(step["id"].as_str().unwrap(), "verify-typecheck-passes");
        assert_eq!(step["type"].as_str().unwrap(), "command");
        assert_eq!(step["phase"].as_str().unwrap(), "verification");
        assert_eq!(step["command"].as_str().unwrap(), "npx tsc --noEmit");
        assert_eq!(step["expected_exit_code"].as_i64().unwrap(), 0);
    }

    #[test]
    fn test_synthesize_ui_bridge_step() {
        let criteria = AcceptanceCriteria {
            goal_summary: "Button renders".to_string(),
            criteria: vec![AcceptanceCriterion {
                id: "button-visible".to_string(),
                description: "Save button is visible".to_string(),
                method: VerificationMethod::UiBridge,
                priority: CriterionPriority::Critical,
                verification_hint: "Assert button exists".to_string(),
                category: "ui-content".to_string(),
                ..Default::default()
            }],
            assumptions: vec![],
            bugfix_context: None,
        };

        let result = synthesize_verification_steps(&criteria, "");
        let step = &result.steps[0];
        assert_eq!(step["type"].as_str().unwrap(), "ui_bridge");
        assert_eq!(step["snapshot_assert"].as_bool().unwrap(), true);
    }

    #[test]
    fn test_synthesize_test_step_uses_detected_runner() {
        let criteria = AcceptanceCriteria {
            goal_summary: "Tests pass".to_string(),
            criteria: vec![AcceptanceCriterion {
                id: "tests-pass".to_string(),
                description: "Unit tests pass".to_string(),
                method: VerificationMethod::Test,
                priority: CriterionPriority::Critical,
                verification_hint: "Run tests".to_string(),
                category: "behavior".to_string(),
                ..Default::default()
            }],
            assumptions: vec![],
            bugfix_context: None,
        };

        // With vitest context
        let result = synthesize_verification_steps(&criteria, "project uses vitest for testing");
        let cmd = result.steps[0]["command"].as_str().unwrap();
        assert!(
            cmd.contains("vitest"),
            "Expected vitest in command: {}",
            cmd
        );

        // With pytest context
        let result = synthesize_verification_steps(&criteria, "python project with pytest");
        let cmd = result.steps[0]["command"].as_str().unwrap();
        assert!(
            cmd.contains("pytest"),
            "Expected pytest in command: {}",
            cmd
        );
    }

    #[test]
    fn test_synthesize_criterion_step_map() {
        let criteria = sample_criteria();
        let result = synthesize_verification_steps(&criteria, "");

        assert!(result.criterion_step_map.contains_key("typecheck-passes"));
        assert!(result.criterion_step_map.contains_key("toggle-renders"));
        assert!(result.criterion_step_map.contains_key("unit-tests-pass"));
        // manual criterion should NOT be in the map
        assert!(!result
            .criterion_step_map
            .contains_key("manual-visual-review"));
    }

    #[test]
    fn test_merge_skips_already_covered() {
        let criteria = sample_criteria();
        let synthesis = synthesize_verification_steps(&criteria, "");

        let mut workflow = json!({
            "verification_steps": [
                {
                    "id": "existing-step",
                    "type": "command",
                    "criterion_ids": ["typecheck-passes"]
                }
            ]
        });

        let initial_count = workflow["verification_steps"].as_array().unwrap().len();
        merge_synthesized_steps(&mut workflow, &synthesis);
        let final_count = workflow["verification_steps"].as_array().unwrap().len();

        // Should have added steps for toggle-renders and unit-tests-pass, but not typecheck-passes
        assert_eq!(final_count, initial_count + 2);
    }

    #[test]
    fn test_merge_creates_verification_steps_array() {
        let criteria = AcceptanceCriteria {
            goal_summary: "Build succeeds".to_string(),
            criteria: vec![AcceptanceCriterion {
                id: "build-ok".to_string(),
                description: "Build succeeds".to_string(),
                method: VerificationMethod::Command,
                priority: CriterionPriority::Critical,
                verification_hint: "Run `npm run build`".to_string(),
                category: "compilation".to_string(),
                ..Default::default()
            }],
            assumptions: vec![],
            bugfix_context: None,
        };

        let synthesis = synthesize_verification_steps(&criteria, "");
        let mut workflow = json!({});

        merge_synthesized_steps(&mut workflow, &synthesis);
        assert!(workflow.get("verification_steps").is_some());
        assert_eq!(workflow["verification_steps"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn test_coverage_report_full_coverage() {
        let criteria = AcceptanceCriteria {
            goal_summary: "All covered".to_string(),
            criteria: vec![AcceptanceCriterion {
                id: "check-a".to_string(),
                description: "Check A".to_string(),
                method: VerificationMethod::Command,
                priority: CriterionPriority::Critical,
                verification_hint: "run something".to_string(),
                category: "general".to_string(),
                ..Default::default()
            }],
            assumptions: vec![],
            bugfix_context: None,
        };

        let workflow = json!({
            "verification_steps": [
                { "criterion_ids": ["check-a"] }
            ]
        });

        let report = coverage_report(&workflow, &criteria);
        assert_eq!(report.total_criteria, 1);
        assert_eq!(report.covered_count, 1);
        assert!(report.uncovered.is_empty());
        assert!((report.coverage_ratio - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn test_coverage_report_partial_coverage() {
        let criteria = sample_criteria();

        let workflow = json!({
            "verification_steps": [
                { "criterion_ids": ["typecheck-passes"] }
            ]
        });

        let report = coverage_report(&workflow, &criteria);
        assert_eq!(report.total_criteria, 4);
        assert_eq!(report.covered_count, 1);
        assert_eq!(report.uncovered.len(), 3);
        assert!(report.coverage_ratio < 0.5);
    }

    #[test]
    fn test_coverage_report_empty_criteria() {
        let criteria = AcceptanceCriteria {
            goal_summary: "Nothing".to_string(),
            criteria: vec![],
            assumptions: vec![],
            bugfix_context: None,
        };
        let workflow = json!({});

        let report = coverage_report(&workflow, &criteria);
        assert_eq!(report.total_criteria, 0);
        assert_eq!(report.covered_count, 0);
        assert!((report.coverage_ratio - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn test_coverage_report_checks_criterion_id_singular() {
        let criteria = AcceptanceCriteria {
            goal_summary: "Check singular field".to_string(),
            criteria: vec![AcceptanceCriterion {
                id: "check-b".to_string(),
                description: "Check B".to_string(),
                method: VerificationMethod::Command,
                priority: CriterionPriority::Critical,
                verification_hint: "run it".to_string(),
                category: "general".to_string(),
                ..Default::default()
            }],
            assumptions: vec![],
            bugfix_context: None,
        };

        // Use singular criterion_id field (as the builder might produce)
        let workflow = json!({
            "verification_steps": [
                { "criterion_id": "check-b" }
            ]
        });

        let report = coverage_report(&workflow, &criteria);
        assert_eq!(report.covered_count, 1);
    }

    #[test]
    fn test_empty_criteria_synthesis() {
        let criteria = AcceptanceCriteria {
            goal_summary: "Nothing to do".to_string(),
            criteria: vec![],
            assumptions: vec![],
            bugfix_context: None,
        };

        let result = synthesize_verification_steps(&criteria, "");
        assert!(result.success);
        assert!(result.steps.is_empty());
        assert!(result.unmapped_criteria.is_empty());
    }

    #[test]
    fn test_detect_test_runner() {
        assert_eq!(detect_test_runner("uses vitest"), TestRunner::Vitest);
        assert_eq!(detect_test_runner("jest config"), TestRunner::Jest);
        assert_eq!(
            detect_test_runner("python pytest project"),
            TestRunner::Pytest
        );
        assert_eq!(detect_test_runner("cargo rust"), TestRunner::Cargo);
        assert_eq!(detect_test_runner("unknown project"), TestRunner::Generic);
    }

    // ── Page Spec Update Tests ──────────────────────────────────────────

    #[test]
    fn test_criterion_to_spec_assertion_structure() {
        let criterion = AcceptanceCriterion {
            id: "toggle-renders".to_string(),
            description: "Dark mode toggle is visible".to_string(),
            method: VerificationMethod::UiBridge,
            priority: CriterionPriority::Critical,
            verification_hint: "Assert element 'toggle-dark-mode' exists".to_string(),
            category: "ui-content".to_string(),
            ..Default::default()
        };

        let assertion = criterion_to_spec_assertion(&criterion);
        assert_eq!(assertion["id"].as_str().unwrap(), "wf-toggle-renders");
        assert_eq!(
            assertion["description"].as_str().unwrap(),
            "Dark mode toggle is visible"
        );
        assert_eq!(assertion["severity"].as_str().unwrap(), "critical");
        assert_eq!(assertion["assertionType"].as_str().unwrap(), "exists");
        assert_eq!(assertion["source"].as_str().unwrap(), "ai-generated");
        assert_eq!(assertion["reviewed"].as_bool().unwrap(), false);
        assert_eq!(assertion["enabled"].as_bool().unwrap(), true);
        assert!(assertion.get("target").is_some());
    }

    #[test]
    fn test_criteria_to_spec_group() {
        let criteria = sample_criteria();
        let group = criteria_to_spec_group(&criteria);

        assert_eq!(group["id"].as_str().unwrap(), "wf-acceptance-criteria");
        assert_eq!(
            group["name"].as_str().unwrap(),
            "Workflow Acceptance Criteria"
        );
        assert_eq!(
            group["description"].as_str().unwrap(),
            "Dark mode toggle works correctly"
        );
        assert_eq!(group["assertions"].as_array().unwrap().len(), 4);
    }

    #[test]
    fn test_priority_to_severity_mapping() {
        assert_eq!(
            priority_to_severity(&CriterionPriority::Critical),
            "critical"
        );
        assert_eq!(
            priority_to_severity(&CriterionPriority::Important),
            "warning"
        );
        assert_eq!(priority_to_severity(&CriterionPriority::Optional), "info");
    }

    #[test]
    fn test_score_spec_match_by_page_url() {
        let spec = json!({
            "description": "Settings page",
            "metadata": {
                "pageUrl": "/settings/dark-mode",
                "component": "DarkModeSettings"
            }
        });

        // Description mentions "settings" which is a URL part
        let score = score_spec_match(&spec, "Fix the dark-mode toggle on settings page");
        assert!(score > 0.3, "Expected score > 0.3, got {}", score);

        // Unrelated description
        let score = score_spec_match(&spec, "Update the database migration script");
        assert!(score < 0.3, "Expected score < 0.3, got {}", score);
    }

    #[test]
    fn test_score_spec_match_by_component() {
        let spec = json!({
            "description": "Workflow builder page",
            "metadata": {
                "pageUrl": "/build/workflows",
                "component": "WorkflowBuilder"
            }
        });

        let score = score_spec_match(&spec, "Fix the WorkflowBuilder save button");
        assert!(score > 0.2, "Expected score > 0.2, got {}", score);
    }

    #[test]
    fn test_update_page_specs_writes_to_matching_file() {
        let dir = tempfile::tempdir().unwrap();
        let spec_path = dir.path().join("settings.spec.uibridge.json");

        let spec_content = json!({
            "version": "1.0.0",
            "description": "Settings page with dark mode toggle",
            "metadata": {
                "pageUrl": "/settings",
                "component": "SettingsPage",
                "tags": ["dark-mode", "settings"]
            },
            "groups": [
                {
                    "id": "existing-group",
                    "name": "Existing",
                    "description": "Existing tests",
                    "category": "element-presence",
                    "assertions": [],
                    "source": "manual"
                }
            ]
        });
        std::fs::write(
            &spec_path,
            serde_json::to_string_pretty(&spec_content).unwrap(),
        )
        .unwrap();

        let criteria = AcceptanceCriteria {
            goal_summary: "Dark mode settings toggle works".to_string(),
            criteria: vec![AcceptanceCriterion {
                id: "toggle-visible".to_string(),
                description: "Toggle is visible on settings page".to_string(),
                method: VerificationMethod::UiBridge,
                priority: CriterionPriority::Critical,
                verification_hint: "Check toggle exists".to_string(),
                category: "ui-content".to_string(),
                ..Default::default()
            }],
            assumptions: vec![],
            bugfix_context: None,
        };

        let result = update_page_specs_from_criteria(
            &criteria,
            "Fix the dark-mode toggle on settings page",
            &[dir.path()],
            0.2,
        );

        assert_eq!(result.specs_updated, 1);
        assert!(result.errors.is_empty());

        // Verify the file was updated
        let updated: Value =
            serde_json::from_str(&std::fs::read_to_string(&spec_path).unwrap()).unwrap();
        let groups = updated["groups"].as_array().unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[1]["id"].as_str().unwrap(), "wf-acceptance-criteria");
        assert_eq!(groups[1]["assertions"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn test_update_page_specs_replaces_existing_group() {
        let dir = tempfile::tempdir().unwrap();
        let spec_path = dir.path().join("settings.spec.uibridge.json");

        let spec_content = json!({
            "version": "1.0.0",
            "description": "Settings page",
            "metadata": { "pageUrl": "/settings", "tags": ["settings"] },
            "groups": [
                {
                    "id": "wf-acceptance-criteria",
                    "name": "Already exists",
                    "description": "Previous criteria",
                    "category": "semantic",
                    "assertions": [{"id": "old-assertion"}],
                    "source": "ai-generated"
                }
            ]
        });
        std::fs::write(
            &spec_path,
            serde_json::to_string_pretty(&spec_content).unwrap(),
        )
        .unwrap();

        let criteria = AcceptanceCriteria {
            goal_summary: "New goal".to_string(),
            criteria: vec![AcceptanceCriterion {
                id: "new-check".to_string(),
                description: "New check".to_string(),
                method: VerificationMethod::Command,
                priority: CriterionPriority::Critical,
                verification_hint: "run it".to_string(),
                category: "compilation".to_string(),
                ..Default::default()
            }],
            assumptions: vec![],
            bugfix_context: None,
        };

        let result = update_page_specs_from_criteria(
            &criteria,
            "Update the settings page",
            &[dir.path()],
            0.2,
        );

        // Should replace the old group, not skip
        assert_eq!(result.specs_updated, 1);

        // Verify the file has the new criteria, not the old
        let updated: Value =
            serde_json::from_str(&std::fs::read_to_string(&spec_path).unwrap()).unwrap();
        let groups = updated["groups"].as_array().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0]["description"].as_str().unwrap(), "New goal");
        assert_eq!(groups[0]["assertions"].as_array().unwrap().len(), 1);
        assert_eq!(
            groups[0]["assertions"][0]["id"].as_str().unwrap(),
            "wf-new-check"
        );
    }

    #[test]
    fn test_update_page_specs_skips_low_scoring() {
        let dir = tempfile::tempdir().unwrap();
        let spec_path = dir.path().join("dashboard.spec.uibridge.json");

        let spec_content = json!({
            "version": "1.0.0",
            "description": "Dashboard overview page",
            "metadata": { "pageUrl": "/dashboard", "component": "Dashboard" },
            "groups": []
        });
        std::fs::write(
            &spec_path,
            serde_json::to_string_pretty(&spec_content).unwrap(),
        )
        .unwrap();

        let criteria = AcceptanceCriteria {
            goal_summary: "Fix auth".to_string(),
            criteria: vec![AcceptanceCriterion {
                id: "auth-check".to_string(),
                description: "Auth check passes".to_string(),
                method: VerificationMethod::Command,
                priority: CriterionPriority::Critical,
                verification_hint: "run auth test".to_string(),
                category: "behavior".to_string(),
                ..Default::default()
            }],
            assumptions: vec![],
            bugfix_context: None,
        };

        // "Fix authentication backend" has nothing to do with "dashboard"
        let result = update_page_specs_from_criteria(
            &criteria,
            "Fix authentication backend service",
            &[dir.path()],
            0.3,
        );

        assert_eq!(result.specs_updated, 0);
    }

    #[test]
    fn test_update_page_specs_empty_criteria() {
        let dir = tempfile::tempdir().unwrap();
        let criteria = AcceptanceCriteria {
            goal_summary: "".to_string(),
            criteria: vec![],
            assumptions: vec![],
            bugfix_context: None,
        };

        let result =
            update_page_specs_from_criteria(&criteria, "some description", &[dir.path()], 0.3);

        assert_eq!(result.specs_updated, 0);
    }

    // ── Classification Tests ────────────────────────────────────────────

    #[test]
    fn test_classify_ops_prompt_skips() {
        let criteria = AcceptanceCriteria {
            goal_summary: "Push changes".to_string(),
            criteria: vec![AcceptanceCriterion {
                id: "push-ok".to_string(),
                description: "Git push succeeds".to_string(),
                method: VerificationMethod::Command,
                priority: CriterionPriority::Critical,
                verification_hint: "git push".to_string(),
                category: "behavior".to_string(),
                ..Default::default()
            }],
            assumptions: vec![],
            bugfix_context: None,
        };

        assert_eq!(
            classify_spec_target(&criteria, "git push the branch and create a PR"),
            SpecTarget::Skip
        );
    }

    #[test]
    fn test_classify_ui_criteria_targets_page() {
        let criteria = AcceptanceCriteria {
            goal_summary: "Toggle renders".to_string(),
            criteria: vec![AcceptanceCriterion {
                id: "toggle-visible".to_string(),
                description: "Toggle is visible".to_string(),
                method: VerificationMethod::UiBridge,
                priority: CriterionPriority::Critical,
                verification_hint: "Assert exists".to_string(),
                category: "ui-content".to_string(),
                ..Default::default()
            }],
            assumptions: vec![],
            bugfix_context: None,
        };

        assert_eq!(
            classify_spec_target(&criteria, "Fix the dark mode toggle"),
            SpecTarget::Page
        );
    }

    #[test]
    fn test_classify_backend_criteria() {
        let criteria = AcceptanceCriteria {
            goal_summary: "WAL checkpoint works".to_string(),
            criteria: vec![AcceptanceCriterion {
                id: "wal-timeout".to_string(),
                description: "WAL checkpoint completes within 5s".to_string(),
                method: VerificationMethod::Command,
                priority: CriterionPriority::Critical,
                verification_hint: "Run checkpoint test".to_string(),
                category: "data-integrity".to_string(),
                ..Default::default()
            }],
            assumptions: vec![],
            bugfix_context: None,
        };

        assert_eq!(
            classify_spec_target(&criteria, "Fix SQLite WAL checkpoint timeout"),
            SpecTarget::Backend
        );
    }

    #[test]
    fn test_ops_skip_prevents_spec_update() {
        let dir = tempfile::tempdir().unwrap();
        let spec_path = dir.path().join("settings.spec.uibridge.json");
        let spec_content = json!({
            "version": "1.0.0",
            "description": "Settings page",
            "metadata": { "pageUrl": "/settings", "tags": ["settings"] },
            "groups": []
        });
        std::fs::write(
            &spec_path,
            serde_json::to_string_pretty(&spec_content).unwrap(),
        )
        .unwrap();

        let criteria = AcceptanceCriteria {
            goal_summary: "Push succeeds".to_string(),
            criteria: vec![AcceptanceCriterion {
                id: "push-ok".to_string(),
                description: "Push succeeds".to_string(),
                method: VerificationMethod::Command,
                priority: CriterionPriority::Critical,
                verification_hint: "git push".to_string(),
                category: "behavior".to_string(),
                ..Default::default()
            }],
            assumptions: vec![],
            bugfix_context: None,
        };

        let result = update_page_specs_from_criteria(
            &criteria,
            "git push the branch to remote",
            &[dir.path()],
            0.2,
        );

        assert_eq!(result.specs_updated, 0);
    }

    #[test]
    fn test_backend_criteria_uses_catchall() {
        let dir = tempfile::tempdir().unwrap();
        let catchall_path = dir.path().join("workflow-criteria.spec.uibridge.json");

        // Pre-create the catch-all file (as it would exist in the repo)
        let catchall_spec = json!({
            "version": "1.0.0",
            "description": "Catch-all spec for workflow acceptance criteria",
            "metadata": { "pageUrl": "/workflows", "tags": ["catch-all"] },
            "groups": []
        });
        std::fs::write(
            &catchall_path,
            serde_json::to_string_pretty(&catchall_spec).unwrap(),
        )
        .unwrap();

        let criteria = AcceptanceCriteria {
            goal_summary: "WAL checkpoint works".to_string(),
            criteria: vec![AcceptanceCriterion {
                id: "wal-ok".to_string(),
                description: "WAL checkpoint completes".to_string(),
                method: VerificationMethod::Command,
                priority: CriterionPriority::Critical,
                verification_hint: "run test".to_string(),
                category: "data-integrity".to_string(),
                ..Default::default()
            }],
            assumptions: vec![],
            bugfix_context: None,
        };

        let result = update_page_specs_from_criteria(
            &criteria,
            "Fix SQLite WAL checkpoint timeout",
            &[dir.path()],
            0.3,
        );

        assert_eq!(result.specs_updated, 1);

        let catchall: Value =
            serde_json::from_str(&std::fs::read_to_string(&catchall_path).unwrap()).unwrap();
        let groups = catchall["groups"].as_array().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0]["id"].as_str().unwrap(), "wf-acceptance-criteria");
    }
}
