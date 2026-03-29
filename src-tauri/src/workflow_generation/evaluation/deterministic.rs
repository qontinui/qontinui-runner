//! Tier 1: Deterministic Scorers
//!
//! Fast, zero-LLM scorers that evaluate verification steps using pattern
//! matching, syntax validation, and structural analysis. Each scorer runs
//! in < 1ms and produces a score + evidence for one evaluation dimension.

use serde_json::Value;

use crate::workflow_generation::specification::AcceptanceCriteria;

use super::{DimensionScore, EvaluationDimension, ScoringTier};

// ============================================================================
// Helpers
// ============================================================================

/// Extract a string field from a step JSON value.
fn get_str(step: &Value, key: &str) -> String {
    step.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// Concatenate name, command, prompt, expected_output, description - all lowercased.
fn get_step_text(step: &Value) -> String {
    let parts: Vec<String> = ["name", "command", "prompt", "expected_output", "description"]
        .iter()
        .map(|k| get_str(step, k))
        .collect();
    parts.join(" ").to_lowercase()
}

/// Clamp a float to the 0.0-1.0 range.
fn clamp01(v: f64) -> f64 {
    v.clamp(0.0, 1.0)
}

// ============================================================================
// Public Entry Point
// ============================================================================

/// Run all Tier 1 deterministic scorers on a step.
pub fn score_step(step: &Value, criteria: Option<&AcceptanceCriteria>) -> Vec<DimensionScore> {
    vec![
        score_determinism(step),
        score_executability(step),
        score_specificity(step),
        score_robustness(step),
        score_coverage_structural(step, criteria),
    ]
}

// ============================================================================
// Scorer 1: Determinism
// ============================================================================

fn score_determinism(step: &Value) -> DimensionScore {
    let step_type = get_str(step, "type");
    let command = get_str(step, "command").to_lowercase();
    let action = get_str(step, "action").to_lowercase();
    let text = get_step_text(step);

    let mut evidence: Vec<String> = Vec::new();
    let mut explanation: String;

    // Base score by step type
    let mut score = match step_type.as_str() {
        "prompt" => {
            explanation = "Prompt steps are non-deterministic by nature".to_string();
            evidence.push("step type: prompt".to_string());
            0.0
        }
        "command" => {
            // Check if command actually validates output (pipes, &&, expected_output)
            let has_output_check = command.contains("|")
                || command.contains("&&")
                || !get_str(step, "expected_output").is_empty();

            if (command.contains("grep") || command.contains("curl")) && has_output_check {
                explanation =
                    "Command checks specific content via grep/curl with output validation".to_string();
                evidence.push("command uses grep/curl with output check".to_string());
                0.8
            } else if command.is_empty() || !has_output_check {
                explanation =
                    "Command runs without output check".to_string();
                evidence.push("no output validation in command".to_string());
                0.5
            } else {
                explanation = "Command step with default determinism".to_string();
                0.6
            }
        }
        "ui_bridge" => {
            if action.contains("assert") {
                explanation = "UI Bridge assert action is deterministic".to_string();
                evidence.push("action: assert".to_string());
                0.8
            } else {
                explanation = "UI Bridge step without assert".to_string();
                0.6
            }
        }
        "test" => {
            explanation = "Test steps are generally deterministic".to_string();
            0.7
        }
        _ => {
            explanation = "Default determinism score for unknown step type".to_string();
            0.6
        }
    };

    // Environment-dependent paths penalty
    let env_patterns = ["$HOME", "$USER", "$TMPDIR", "${HOME}", "${USER}", "${TMPDIR}"];
    if env_patterns.iter().any(|p| text.contains(&p.to_lowercase())) {
        score -= 0.2;
        evidence.push("environment-dependent path detected".to_string());
        explanation = format!("{} (penalized: env-dependent path)", explanation);
    }

    // Timestamp/date penalty — check command field only to avoid false positives
    // from words like "update", "validate", "candidate" containing "date"
    let date_patterns = ["$(date", "strftime", "now()", " date ", "date +"];
    if date_patterns.iter().any(|p| command.contains(p)) {
        score -= 0.2;
        evidence.push("timestamp/date reference detected in command".to_string());
        explanation = format!("{} (penalized: date/time dependency)", explanation);
    }

    // Randomness penalty — use specific patterns to avoid false positives
    // from "temperature", "template", "attempt" containing "temp"
    let random_patterns = ["uuid", "random", "$random", "mktemp"];
    if random_patterns.iter().any(|p| text.contains(p)) {
        score -= 0.2;
        evidence.push("randomness reference detected".to_string());
        explanation = format!("{} (penalized: randomness)", explanation);
    }

    DimensionScore {
        dimension: EvaluationDimension::Determinism,
        score: clamp01(score),
        confidence: 0.8,
        tier: ScoringTier::Deterministic,
        explanation: Some(explanation),
        evidence,
    }
}

// ============================================================================
// Scorer 2: Executability
// ============================================================================

fn score_executability(step: &Value) -> DimensionScore {
    let step_type = get_str(step, "type");
    let command = get_str(step, "command");
    let text = get_step_text(step);
    let url = get_str(step, "url");

    let mut evidence: Vec<String> = Vec::new();
    let mut explanation: String;

    // Prompt steps are always executable (run through AI)
    if step_type == "prompt" {
        return DimensionScore {
            dimension: EvaluationDimension::Executability,
            score: 1.0,
            confidence: 0.8,
            tier: ScoringTier::Deterministic,
            explanation: Some("Prompt steps always execute through AI".to_string()),
            evidence: vec![],
        };
    }

    // Placeholder detection — check command/url fields for path placeholders,
    // and full text only for unambiguous placeholders
    let command_lower = command.to_lowercase();
    let path_placeholders = [
        "/path/to/project",
        "/path/to/",
        "<placeholder>",
        "insert_",
        "replace_me",
    ];
    // These are checked against command only to avoid false positives
    // on step names like "check todo completion"
    let command_only_placeholders = ["todo", "fixme", "xxx"];
    let mut placeholder_patterns: Vec<(&str, bool)> = path_placeholders
        .iter()
        .map(|p| (*p, false)) // check against full text
        .collect();
    for p in &command_only_placeholders {
        placeholder_patterns.push((p, true)); // check against command only
    }
    // "your-" checked against full text (unlikely in legitimate names)
    placeholder_patterns.push(("your-", false));
    // example.com is only a placeholder if not in a test step
    if step_type != "test" {
        placeholder_patterns.push(("example.com", false));
    }

    for (pattern, command_only) in &placeholder_patterns {
        let matches = if *command_only {
            command_lower.contains(pattern)
        } else {
            text.contains(pattern)
        };
        if matches {
            evidence.push(format!("placeholder detected: {}", pattern));
            return DimensionScore {
                dimension: EvaluationDimension::Executability,
                score: 0.0,
                confidence: 0.8,
                tier: ScoringTier::Deterministic,
                explanation: Some(format!(
                    "Step contains placeholder value: {}",
                    pattern
                )),
                evidence,
            };
        }
    }

    let mut score = 0.9;
    explanation = "Step appears executable".to_string();

    // Empty command check
    if (step_type == "command" || step_type.is_empty()) && command.trim().is_empty() {
        return DimensionScore {
            dimension: EvaluationDimension::Executability,
            score: 0.0,
            confidence: 0.8,
            tier: ScoringTier::Deterministic,
            explanation: Some("Empty command cannot be executed".to_string()),
            evidence: vec!["empty command".to_string()],
        };
    }

    // Unbalanced quotes check
    let single_quotes = command.chars().filter(|c| *c == '\'').count();
    let double_quotes = command.chars().filter(|c| *c == '"').count();
    if single_quotes % 2 != 0 || double_quotes % 2 != 0 {
        score -= 0.3;
        evidence.push("unbalanced quotes in command".to_string());
        explanation = "Command has unbalanced quotes".to_string();
    }

    // URL validation for curl/wget steps
    let command_lower = command.to_lowercase();
    if command_lower.contains("curl") || command_lower.contains("wget") || step_type == "http_status" {
        let check_url = if !url.is_empty() {
            url.clone()
        } else {
            // Try to extract URL from command
            command
                .split_whitespace()
                .find(|w| w.starts_with("http://") || w.starts_with("https://"))
                .unwrap_or("")
                .to_string()
        };

        if !check_url.is_empty()
            && !check_url.starts_with("http://")
            && !check_url.starts_with("https://")
        {
            score -= 0.3;
            evidence.push(format!("invalid URL format: {}", check_url));
            explanation = "URL does not start with http:// or https://".to_string();
        }
    }

    DimensionScore {
        dimension: EvaluationDimension::Executability,
        score: clamp01(score),
        confidence: 0.8,
        tier: ScoringTier::Deterministic,
        explanation: Some(explanation),
        evidence,
    }
}

// ============================================================================
// Scorer 3: Specificity
// ============================================================================

fn score_specificity(step: &Value) -> DimensionScore {
    let step_type = get_str(step, "type");
    let command = get_str(step, "command").to_lowercase();
    let action = get_str(step, "action").to_lowercase();
    let expected_output = get_str(step, "expected_output");

    let mut evidence: Vec<String> = Vec::new();
    let explanation: String;

    // Prompt steps are inherently imprecise
    if step_type == "prompt" {
        return DimensionScore {
            dimension: EvaluationDimension::Specificity,
            score: 0.3,
            confidence: 0.8,
            tier: ScoringTier::Deterministic,
            explanation: Some("Prompt steps are inherently imprecise".to_string()),
            evidence: vec!["step type: prompt".to_string()],
        };
    }

    // Anti-pattern: grep "error" without specificity
    if command.contains("grep")
        && command.contains("error")
        && !command.contains("-c")
        && !command.contains("^")
    {
        // Check that it's a generic grep for "error" without a specific pattern
        evidence.push("grep 'error' without specific pattern".to_string());
        return DimensionScore {
            dimension: EvaluationDimension::Specificity,
            score: 0.3,
            confidence: 0.8,
            tier: ScoringTier::Deterministic,
            explanation: Some(
                "grep for 'error' may match false positives like 'no error found'".to_string(),
            ),
            evidence,
        };
    }

    // Anti-pattern: curl without status code check
    if command.contains("curl") && command.contains("-s") {
        if !command.contains("-w")
            && !command.contains("--write-out")
            && !command.contains("grep")
            && !command.contains("|")
        {
            evidence.push("curl -s without status code check".to_string());
            return DimensionScore {
                dimension: EvaluationDimension::Specificity,
                score: 0.4,
                confidence: 0.8,
                tier: ScoringTier::Deterministic,
                explanation: Some(
                    "curl without status code or output check may miss failures".to_string(),
                ),
                evidence,
            };
        }
    }

    // Anti-pattern: test -f/-e without content check
    if (command.contains("test -f") || command.contains("test -e"))
        && !command.contains("&&")
        && !command.contains("|")
    {
        evidence.push("file existence check without content verification".to_string());
        return DimensionScore {
            dimension: EvaluationDimension::Specificity,
            score: 0.5,
            confidence: 0.8,
            tier: ScoringTier::Deterministic,
            explanation: Some(
                "File existence check without content verification".to_string(),
            ),
            evidence,
        };
    }

    // Anti-pattern: wc -l without comparison
    if command.contains("wc -l")
        && !command.contains("-gt")
        && !command.contains("-lt")
        && !command.contains("-eq")
    {
        evidence.push("wc -l without numeric comparison".to_string());
        return DimensionScore {
            dimension: EvaluationDimension::Specificity,
            score: 0.4,
            confidence: 0.8,
            tier: ScoringTier::Deterministic,
            explanation: Some(
                "Line count without threshold comparison is not specific".to_string(),
            ),
            evidence,
        };
    }

    // Anti-pattern: UI Bridge assert with very short expected text
    if step_type == "ui_bridge" && action.contains("assert") {
        if expected_output.len() < 3 && !expected_output.is_empty() {
            evidence.push(format!(
                "very short expected text: '{}'",
                expected_output
            ));
            return DimensionScore {
                dimension: EvaluationDimension::Specificity,
                score: 0.5,
                confidence: 0.8,
                tier: ScoringTier::Deterministic,
                explanation: Some(
                    "UI Bridge assert with very short expected text may false-positive".to_string(),
                ),
                evidence,
            };
        }
    }

    explanation = "Step has adequate specificity".to_string();

    DimensionScore {
        dimension: EvaluationDimension::Specificity,
        score: 0.8,
        confidence: 0.8,
        tier: ScoringTier::Deterministic,
        explanation: Some(explanation),
        evidence,
    }
}

// ============================================================================
// Scorer 4: Robustness
// ============================================================================

fn score_robustness(step: &Value) -> DimensionScore {
    let step_type = get_str(step, "type");
    let command = get_str(step, "command").to_lowercase();

    let mut score: f64 = 0.5;
    let mut evidence: Vec<String> = Vec::new();
    let mut explanations: Vec<String> = Vec::new();

    // Retry bonus
    let retry_count = step
        .get("retry_count")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    if retry_count > 0 {
        score += 0.2;
        evidence.push(format!("retry_count: {}", retry_count));
        explanations.push("has retry configured".to_string());
    }

    // Timeout bonus
    let has_timeout = step
        .get("timeout_seconds")
        .and_then(|v| v.as_u64())
        .is_some();
    if has_timeout {
        score += 0.1;
        evidence.push("timeout_seconds configured".to_string());
        explanations.push("has timeout configured".to_string());
    }

    // Dependencies bonus
    let has_depends = step
        .get("depends_on")
        .and_then(|v| v.as_array())
        .map(|arr| !arr.is_empty())
        .unwrap_or(false);
    if has_depends {
        score += 0.1;
        evidence.push("depends_on configured".to_string());
        explanations.push("has dependency chain".to_string());
    }

    // Network-dependent without retry penalty
    let is_network = command.contains("curl")
        || command.contains("wget")
        || step_type == "http_status"
        || step_type == "http";
    if is_network && retry_count == 0 {
        score -= 0.3;
        evidence.push("network-dependent step without retry".to_string());
        explanations.push("network step lacks retry".to_string());
    }

    // UI Bridge without retry penalty
    if step_type == "ui_bridge" && retry_count == 0 {
        score -= 0.2;
        evidence.push("ui_bridge step without retry".to_string());
        explanations.push("UI Bridge step lacks retry".to_string());
    }

    let explanation = if explanations.is_empty() {
        "Base robustness score".to_string()
    } else {
        explanations.join("; ")
    };

    DimensionScore {
        dimension: EvaluationDimension::Robustness,
        score: clamp01(score),
        confidence: 0.8,
        tier: ScoringTier::Deterministic,
        explanation: Some(explanation),
        evidence,
    }
}

// ============================================================================
// Scorer 5: Coverage (Structural)
// ============================================================================

fn score_coverage_structural(
    step: &Value,
    criteria: Option<&AcceptanceCriteria>,
) -> DimensionScore {
    let mut evidence: Vec<String> = Vec::new();

    // Extract the step's criterion_id or first entry of criterion_ids
    let criterion_id = get_str(step, "criterion_id");
    let criterion_id = if criterion_id.is_empty() {
        step.get("criterion_ids")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    } else {
        criterion_id
    };

    // No criterion linked
    if criterion_id.is_empty() {
        return DimensionScore {
            dimension: EvaluationDimension::Coverage,
            score: 0.3,
            confidence: 0.5,
            tier: ScoringTier::Deterministic,
            explanation: Some("No criterion mapped to this step".to_string()),
            evidence: vec!["no criterion_id or criterion_ids".to_string()],
        };
    }

    // Try to find criterion in the criteria list
    let criteria = match criteria {
        Some(c) => c,
        None => {
            evidence.push(format!("criterion_id: {}", criterion_id));
            return DimensionScore {
                dimension: EvaluationDimension::Coverage,
                score: 0.5,
                confidence: 0.5,
                tier: ScoringTier::Deterministic,
                explanation: Some(
                    "Criterion ID present but no criteria list provided".to_string(),
                ),
                evidence,
            };
        }
    };

    let criterion = criteria.criteria.iter().find(|c| c.id == criterion_id);
    let criterion = match criterion {
        Some(c) => c,
        None => {
            evidence.push(format!("criterion_id: {} (not found in criteria list)", criterion_id));
            return DimensionScore {
                dimension: EvaluationDimension::Coverage,
                score: 0.5,
                confidence: 0.5,
                tier: ScoringTier::Deterministic,
                explanation: Some(
                    "Criterion ID present but not found in criteria list".to_string(),
                ),
                evidence,
            };
        }
    };

    // Extract keywords from criterion description (words > 3 chars)
    let keywords: Vec<&str> = criterion
        .description
        .split_whitespace()
        .filter(|w| w.len() > 3)
        .collect();

    if keywords.is_empty() {
        evidence.push("criterion description has no keywords > 3 chars".to_string());
        return DimensionScore {
            dimension: EvaluationDimension::Coverage,
            score: 0.5,
            confidence: 0.5,
            tier: ScoringTier::Deterministic,
            explanation: Some("Cannot compute keyword overlap - no keywords".to_string()),
            evidence,
        };
    }

    // Extract step text and count keyword matches
    let step_text = get_step_text(step);
    let matches: usize = keywords
        .iter()
        .filter(|kw| step_text.contains(&kw.to_lowercase()))
        .count();

    let ratio = (matches as f64 / keywords.len() as f64).min(1.0);
    let score = 0.3 + 0.7 * ratio;

    evidence.push(format!(
        "keyword overlap: {}/{} keywords matched",
        matches,
        keywords.len()
    ));
    evidence.push(format!("criterion: {}", criterion_id));

    DimensionScore {
        dimension: EvaluationDimension::Coverage,
        score: clamp01(score),
        confidence: 0.5,
        tier: ScoringTier::Deterministic,
        explanation: Some(format!(
            "Structural keyword overlap: {:.0}% of criterion keywords found in step",
            ratio * 100.0
        )),
        evidence,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    use crate::workflow_generation::specification::{
        AcceptanceCriteria, AcceptanceCriterion, CriterionPriority, VerificationMethod,
    };

    // ========================================================================
    // Helpers
    // ========================================================================

    fn make_criteria(criteria: Vec<AcceptanceCriterion>) -> AcceptanceCriteria {
        AcceptanceCriteria {
            goal_summary: "test".into(),
            criteria,
            assumptions: vec![],
        }
    }

    fn get_score(
        scores: &[super::super::DimensionScore],
        dim: super::super::EvaluationDimension,
    ) -> f64 {
        scores
            .iter()
            .find(|s| s.dimension == dim)
            .map(|s| s.score)
            .unwrap_or(-1.0)
    }

    // ========================================================================
    // Original tests (preserved)
    // ========================================================================

    #[test]
    fn test_prompt_step_determinism_zero() {
        let step = json!({"type": "prompt", "name": "check via AI"});
        let score = score_determinism(&step);
        assert_eq!(score.score, 0.0);
    }

    #[test]
    fn test_placeholder_scores_zero_executability() {
        let step = json!({"type": "command", "command": "ls /path/to/project"});
        let score = score_executability(&step);
        assert_eq!(score.score, 0.0);
        assert!(score.evidence.iter().any(|e| e.contains("placeholder")));
    }

    #[test]
    fn test_empty_command_scores_zero() {
        let step = json!({"type": "command", "command": ""});
        let score = score_executability(&step);
        assert_eq!(score.score, 0.0);
    }

    #[test]
    fn test_grep_error_low_specificity() {
        let step = json!({"type": "command", "command": "grep error log.txt"});
        let score = score_specificity(&step);
        assert_eq!(score.score, 0.3);
    }

    #[test]
    fn test_robustness_with_retry() {
        let step = json!({"type": "command", "command": "echo hi", "retry_count": 3});
        let score = score_robustness(&step);
        assert!(score.score >= 0.7);
    }

    #[test]
    fn test_coverage_no_criterion() {
        let step = json!({"type": "command", "command": "echo hi"});
        let score = score_coverage_structural(&step, None);
        assert_eq!(score.score, 0.3);
    }

    #[test]
    fn test_score_step_returns_five_scores() {
        let step = json!({"type": "command", "command": "echo hi"});
        let scores = score_step(&step, None);
        assert_eq!(scores.len(), 5);
    }

    // ========================================================================
    // score_determinism — edge cases
    // ========================================================================

    #[test]
    fn determinism_prompt_step_is_zero() {
        let step = json!({"type": "prompt", "prompt": "check something"});
        let scores = score_step(&step, None);
        assert_eq!(
            get_score(&scores, EvaluationDimension::Determinism),
            0.0,
            "Prompt steps should always score 0.0 for determinism"
        );
    }

    #[test]
    fn determinism_command_with_pipe_is_high() {
        let step = json!({"type": "command", "command": "curl http://localhost:3000 | grep 'OK'"});
        let scores = score_step(&step, None);
        assert_eq!(
            get_score(&scores, EvaluationDimension::Determinism),
            0.8,
            "curl with pipe to grep should be highly deterministic"
        );
    }

    #[test]
    fn determinism_command_without_output_check() {
        let step = json!({"type": "command", "command": "ls /tmp"});
        let scores = score_step(&step, None);
        assert_eq!(
            get_score(&scores, EvaluationDimension::Determinism),
            0.5,
            "Command without output validation should score 0.5"
        );
    }

    #[test]
    fn determinism_env_dependent_penalty() {
        let step = json!({"type": "command", "command": "cat $HOME/.config | grep setting"});
        let scores = score_step(&step, None);
        let det = get_score(&scores, EvaluationDimension::Determinism);
        assert!(
            det < 0.8,
            "Environment-dependent command should be penalized below 0.8, got {}",
            det
        );
    }

    #[test]
    fn determinism_mktemp_penalty() {
        let step = json!({"type": "command", "command": "mktemp && echo test > $tmpfile"});
        let scores = score_step(&step, None);
        let det = get_score(&scores, EvaluationDimension::Determinism);
        // mktemp triggers randomness penalty (-0.2)
        assert!(
            det < 0.8,
            "mktemp command should be penalized for randomness, got {}",
            det
        );
    }

    #[test]
    fn determinism_ui_bridge_assert_is_high() {
        let step = json!({"type": "ui_bridge", "action": "assert", "expected_output": "Welcome"});
        let scores = score_step(&step, None);
        assert_eq!(
            get_score(&scores, EvaluationDimension::Determinism),
            0.8,
            "UI Bridge assert action should score 0.8"
        );
    }

    #[test]
    fn determinism_test_step() {
        let step = json!({"type": "test", "command": "npx playwright test"});
        let scores = score_step(&step, None);
        assert_eq!(
            get_score(&scores, EvaluationDimension::Determinism),
            0.7,
            "Test steps should score 0.7 for determinism"
        );
    }

    // ========================================================================
    // score_executability — edge cases
    // ========================================================================

    #[test]
    fn executability_placeholder_path() {
        let step = json!({"type": "command", "command": "/path/to/project/run.sh"});
        let scores = score_step(&step, None);
        assert_eq!(
            get_score(&scores, EvaluationDimension::Executability),
            0.0,
            "Placeholder path should score 0.0 executability"
        );
    }

    #[test]
    fn executability_empty_command() {
        let step = json!({"type": "command", "command": ""});
        let scores = score_step(&step, None);
        assert_eq!(
            get_score(&scores, EvaluationDimension::Executability),
            0.0,
            "Empty command should score 0.0 executability"
        );
    }

    #[test]
    fn executability_prompt_always_executable() {
        let step = json!({"type": "prompt", "prompt": "do something"});
        let scores = score_step(&step, None);
        assert_eq!(
            get_score(&scores, EvaluationDimension::Executability),
            1.0,
            "Prompt steps should always be executable (1.0)"
        );
    }

    #[test]
    fn executability_unbalanced_quotes() {
        let step = json!({"type": "command", "command": "echo 'hello"});
        let scores = score_step(&step, None);
        let exec = get_score(&scores, EvaluationDimension::Executability);
        assert!(
            exec < 0.9,
            "Unbalanced quotes should penalize executability below 0.9, got {}",
            exec
        );
    }

    #[test]
    fn executability_valid_command() {
        let step = json!({"type": "command", "command": "npm run test"});
        let scores = score_step(&step, None);
        assert_eq!(
            get_score(&scores, EvaluationDimension::Executability),
            0.9,
            "Valid command should score 0.9 executability"
        );
    }

    #[test]
    fn executability_invalid_url() {
        // curl with a URL that doesn't start with http:// or https://
        // The URL extraction only looks for http:// or https:// prefixed tokens,
        // so ftp://invalid won't be extracted from the command string.
        // But if passed via the url field, it will be checked.
        let step = json!({"type": "command", "command": "curl ftp://invalid", "url": "ftp://invalid"});
        let scores = score_step(&step, None);
        let exec = get_score(&scores, EvaluationDimension::Executability);
        assert!(
            exec < 0.9,
            "Invalid URL scheme should penalize executability, got {}",
            exec
        );
    }

    #[test]
    fn executability_todo_in_name_not_flagged() {
        // The name contains "todo" but the command doesn't —
        // command_only_placeholders only check the command field
        let step = json!({
            "type": "command",
            "name": "check todo items",
            "command": "wc -l report.txt"
        });
        let scores = score_step(&step, None);
        let exec = get_score(&scores, EvaluationDimension::Executability);
        assert!(
            exec > 0.0,
            "Name containing 'todo' should NOT flag executability as 0.0 when command is clean, got {}",
            exec
        );
    }

    // ========================================================================
    // score_specificity — edge cases
    // ========================================================================

    #[test]
    fn specificity_generic_grep_error() {
        let step = json!({"type": "command", "command": "grep error output.log"});
        let scores = score_step(&step, None);
        assert_eq!(
            get_score(&scores, EvaluationDimension::Specificity),
            0.3,
            "Generic grep for 'error' should score 0.3 specificity"
        );
    }

    #[test]
    fn specificity_specific_grep_pattern() {
        let step = json!({"type": "command", "command": "grep '^ERROR:' output.log"});
        let scores = score_step(&step, None);
        assert_eq!(
            get_score(&scores, EvaluationDimension::Specificity),
            0.8,
            "Specific grep pattern with ^ should score 0.8 (passes generic grep anti-pattern check)"
        );
    }

    #[test]
    fn specificity_curl_without_check() {
        let step = json!({"type": "command", "command": "curl -s http://localhost:3000"});
        let scores = score_step(&step, None);
        assert_eq!(
            get_score(&scores, EvaluationDimension::Specificity),
            0.4,
            "curl -s without output check should score 0.4"
        );
    }

    #[test]
    fn specificity_curl_with_grep() {
        let step = json!({"type": "command", "command": "curl -s http://localhost:3000 | grep 'OK'"});
        let scores = score_step(&step, None);
        assert_eq!(
            get_score(&scores, EvaluationDimension::Specificity),
            0.8,
            "curl -s piped to grep should score 0.8 (has output check via pipe)"
        );
    }

    #[test]
    fn specificity_file_existence_only() {
        let step = json!({"type": "command", "command": "test -f config.json"});
        let scores = score_step(&step, None);
        assert_eq!(
            get_score(&scores, EvaluationDimension::Specificity),
            0.5,
            "File existence check without content verification should score 0.5"
        );
    }

    #[test]
    fn specificity_file_with_content_check() {
        let step = json!({"type": "command", "command": "test -f config.json && grep 'port' config.json"});
        let scores = score_step(&step, None);
        assert_eq!(
            get_score(&scores, EvaluationDimension::Specificity),
            0.8,
            "File existence + content check should score 0.8 (passes file-only anti-pattern)"
        );
    }

    #[test]
    fn specificity_prompt_is_imprecise() {
        let step = json!({"type": "prompt"});
        let scores = score_step(&step, None);
        assert_eq!(
            get_score(&scores, EvaluationDimension::Specificity),
            0.3,
            "Prompt steps should score 0.3 specificity (inherently imprecise)"
        );
    }

    #[test]
    fn specificity_wc_without_threshold() {
        let step = json!({"type": "command", "command": "wc -l output.txt"});
        let scores = score_step(&step, None);
        assert_eq!(
            get_score(&scores, EvaluationDimension::Specificity),
            0.4,
            "wc -l without numeric comparison should score 0.4"
        );
    }

    #[test]
    fn specificity_short_ui_assert() {
        let step = json!({"type": "ui_bridge", "action": "assert", "expected_output": "OK"});
        let scores = score_step(&step, None);
        assert_eq!(
            get_score(&scores, EvaluationDimension::Specificity),
            0.5,
            "UI Bridge assert with very short expected text ('OK', 2 chars) should score 0.5"
        );
    }

    // ========================================================================
    // score_robustness — edge cases
    // ========================================================================

    #[test]
    fn robustness_base_score() {
        let step = json!({"type": "command", "command": "echo test"});
        let scores = score_step(&step, None);
        assert_eq!(
            get_score(&scores, EvaluationDimension::Robustness),
            0.5,
            "Base robustness should be 0.5"
        );
    }

    #[test]
    fn robustness_with_retry() {
        let step = json!({"type": "command", "command": "echo test", "retry_count": 3});
        let scores = score_step(&step, None);
        assert_eq!(
            get_score(&scores, EvaluationDimension::Robustness),
            0.7,
            "Retry count should add 0.2 to base 0.5"
        );
    }

    #[test]
    fn robustness_with_retry_and_timeout() {
        let step = json!({"type": "command", "command": "echo test", "retry_count": 3, "timeout_seconds": 30});
        let scores = score_step(&step, None);
        let score = get_score(&scores, EvaluationDimension::Robustness);
        assert!(
            (score - 0.8).abs() < 0.01,
            "Retry (+0.2) and timeout (+0.1) should give ~0.8, got {}",
            score
        );
    }

    #[test]
    fn robustness_network_without_retry() {
        let step = json!({"type": "command", "command": "curl http://localhost"});
        let scores = score_step(&step, None);
        assert_eq!(
            get_score(&scores, EvaluationDimension::Robustness),
            0.2,
            "Network step without retry should be 0.5 - 0.3 = 0.2"
        );
    }

    #[test]
    fn robustness_network_with_retry() {
        let step = json!({"type": "command", "command": "curl http://localhost", "retry_count": 2});
        let scores = score_step(&step, None);
        // 0.5 (base) + 0.2 (retry) = 0.7, no network penalty since retry > 0
        assert_eq!(
            get_score(&scores, EvaluationDimension::Robustness),
            0.7,
            "Network step with retry should be 0.5 + 0.2 = 0.7 (no network penalty)"
        );
    }

    #[test]
    fn robustness_ui_bridge_without_retry() {
        let step = json!({"type": "ui_bridge", "action": "click"});
        let scores = score_step(&step, None);
        assert_eq!(
            get_score(&scores, EvaluationDimension::Robustness),
            0.3,
            "UI Bridge step without retry should be 0.5 - 0.2 = 0.3"
        );
    }

    // ========================================================================
    // score_coverage_structural — edge cases
    // ========================================================================

    #[test]
    fn coverage_no_criterion() {
        let step = json!({"type": "command", "command": "echo test"});
        let scores = score_step(&step, None);
        assert_eq!(
            get_score(&scores, EvaluationDimension::Coverage),
            0.3,
            "Step with no criterion_id and no criteria should score 0.3"
        );
    }

    #[test]
    fn coverage_criterion_not_in_list() {
        let step = json!({"type": "command", "command": "echo test", "criterion_id": "unknown-id"});
        let criteria = make_criteria(vec![]);
        let scores = score_step(&step, Some(&criteria));
        assert_eq!(
            get_score(&scores, EvaluationDimension::Coverage),
            0.5,
            "Criterion ID present but not found in empty criteria list should score 0.5"
        );
    }

    #[test]
    fn coverage_good_keyword_match() {
        let criterion = AcceptanceCriterion {
            id: "server-responds".into(),
            description: "Server returns HTTP 200 with valid JSON response body".into(),
            method: VerificationMethod::Command,
            priority: CriterionPriority::Critical,
            verification_hint: "curl the server".into(),
            category: "behavior".into(),
        };
        let criteria = make_criteria(vec![criterion]);
        // Command contains keywords from the criterion description:
        // "server", "returns", "http", "valid", "json", "response", "body"
        let step = json!({
            "type": "command",
            "command": "curl -s http://localhost:3000 | grep 'valid'",
            "criterion_id": "server-responds",
            "name": "check server json response"
        });
        let scores = score_step(&step, Some(&criteria));
        let cov = get_score(&scores, EvaluationDimension::Coverage);
        assert!(
            cov > 0.6,
            "Step with good keyword overlap should score > 0.6, got {}",
            cov
        );
    }

    #[test]
    fn coverage_no_keyword_match() {
        let criterion = AcceptanceCriterion {
            id: "typecheck-passes".into(),
            description: "TypeScript compilation succeeds with zero errors".into(),
            method: VerificationMethod::Command,
            priority: CriterionPriority::Critical,
            verification_hint: "run tsc".into(),
            category: "compilation".into(),
        };
        let criteria = make_criteria(vec![criterion]);
        // Command has no overlap with criterion keywords
        let step = json!({
            "type": "command",
            "command": "ls -la /tmp",
            "criterion_id": "typecheck-passes"
        });
        let scores = score_step(&step, Some(&criteria));
        let cov = get_score(&scores, EvaluationDimension::Coverage);
        assert_eq!(
            cov, 0.3,
            "Step with zero keyword overlap should score 0.3, got {}",
            cov
        );
    }

    // ========================================================================
    // score_step integration — cross-dimension sanity checks
    // ========================================================================

    #[test]
    fn score_step_all_dimensions_present() {
        let step = json!({"type": "command", "command": "npm test"});
        let scores = score_step(&step, None);
        assert_eq!(scores.len(), 5);
        assert!(scores.iter().any(|s| s.dimension == EvaluationDimension::Determinism));
        assert!(scores.iter().any(|s| s.dimension == EvaluationDimension::Executability));
        assert!(scores.iter().any(|s| s.dimension == EvaluationDimension::Specificity));
        assert!(scores.iter().any(|s| s.dimension == EvaluationDimension::Robustness));
        assert!(scores.iter().any(|s| s.dimension == EvaluationDimension::Coverage));
    }

    #[test]
    fn score_step_all_scores_in_range() {
        let steps = vec![
            json!({"type": "prompt", "prompt": "check it"}),
            json!({"type": "command", "command": "echo hi"}),
            json!({"type": "command", "command": "curl http://localhost | grep OK", "retry_count": 3}),
            json!({"type": "ui_bridge", "action": "assert", "expected_output": "Welcome"}),
            json!({"type": "test", "command": "npx playwright test"}),
            json!({"type": "command", "command": "mktemp && cat $HOME/.config"}),
            json!({"type": "command", "command": ""}),
        ];
        for step in &steps {
            let scores = score_step(step, None);
            for ds in &scores {
                assert!(
                    ds.score >= 0.0 && ds.score <= 1.0,
                    "Score out of range for {:?} on step {:?}: {}",
                    ds.dimension,
                    step,
                    ds.score
                );
            }
        }
    }

    #[test]
    fn score_step_all_tiers_deterministic() {
        let step = json!({"type": "command", "command": "echo hi"});
        let scores = score_step(&step, None);
        for ds in &scores {
            assert_eq!(
                ds.tier,
                ScoringTier::Deterministic,
                "All Tier 1 scores should be ScoringTier::Deterministic"
            );
        }
    }

    // ========================================================================
    // Compound penalty stacking
    // ========================================================================

    #[test]
    fn determinism_multiple_penalties_stack() {
        // mktemp (randomness -0.2) + $HOME (env -0.2) on a command without output check (0.5 base)
        let step = json!({"type": "command", "command": "mktemp $HOME/test"});
        let score = score_determinism(&step);
        // 0.5 (no output check) - 0.2 (env) - 0.2 (random) = 0.1
        assert!(
            score.score <= 0.1 + f64::EPSILON,
            "Multiple penalties should stack, got {}",
            score.score
        );
    }

    #[test]
    fn determinism_clamped_at_zero() {
        // Even with heavy penalties, score should never go below 0.0
        let step = json!({"type": "prompt", "command": "mktemp $HOME/$(date +%s)"});
        let score = score_determinism(&step);
        assert_eq!(
            score.score, 0.0,
            "Score should be clamped at 0.0 even with stacked penalties"
        );
    }

    // ========================================================================
    // Evidence and explanation checks
    // ========================================================================

    #[test]
    fn determinism_env_penalty_has_evidence() {
        let step = json!({"type": "command", "command": "cat $HOME/.bashrc"});
        let score = score_determinism(&step);
        assert!(
            score.evidence.iter().any(|e| e.contains("environment")),
            "Environment penalty should produce evidence mentioning 'environment'"
        );
    }

    #[test]
    fn executability_placeholder_has_evidence() {
        let step = json!({"type": "command", "command": "cat /path/to/project/config"});
        let score = score_executability(&step);
        assert!(
            score.evidence.iter().any(|e| e.contains("placeholder")),
            "Placeholder detection should produce evidence mentioning 'placeholder'"
        );
    }

    #[test]
    fn robustness_network_penalty_has_evidence() {
        let step = json!({"type": "command", "command": "wget http://example.org/file"});
        let score = score_robustness(&step);
        assert!(
            score.evidence.iter().any(|e| e.contains("network")),
            "Network penalty should produce evidence mentioning 'network'"
        );
    }
}
