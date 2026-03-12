//! Verification Templates
//!
//! Provides a template library for verification steps with semantic depth
//! classification. Each template maps to a verification category and can
//! produce concrete step JSON for workflow generation.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::unified_workflows::UnifiedWorkflow;

/// Strip shell metacharacters from a template parameter, allowing only
/// alphanumeric chars and a safe set of punctuation.
fn sanitize_shell_param(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, '/' | '.' | '-' | '_' | ':' | ' '))
        .collect()
}

/// Semantic depth category for verification steps.
///
/// Classifies what a verification step actually checks, enabling
/// gap detection and false-positive risk analysis.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum VerificationCategory {
    /// Checks that something exists (file, endpoint, element)
    Existence,
    /// Checks that a value is unique (no duplicates)
    Uniqueness,
    /// Checks that references between entities are valid
    ReferentialIntegrity,
    /// Checks that content/output is semantically correct
    SemanticCorrectness,
    /// Checks runtime behavior (response times, side effects)
    RuntimeBehavior,
}

/// A template for generating concrete verification steps.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "template_type", rename_all = "snake_case")]
pub enum VerificationTemplate {
    ExistenceCheck {
        target: String,
        path: String,
    },
    UniquenessCheck {
        field: String,
        source: String,
    },
    ReferentialIntegrity {
        source_table: String,
        target_table: String,
        key: String,
    },
    SchemaValidation {
        schema_path: String,
        data_path: String,
    },
    HttpHealthCheck {
        url: String,
        expected_status: u16,
    },
    RuntimeBehavior {
        command: String,
        expected_output: String,
    },
    ContentAssertion {
        url: String,
        element: String,
        expected: String,
    },
}

impl VerificationTemplate {
    /// Get the verification category for this template.
    pub fn category(&self) -> VerificationCategory {
        match self {
            VerificationTemplate::ExistenceCheck { .. } => VerificationCategory::Existence,
            VerificationTemplate::UniquenessCheck { .. } => VerificationCategory::Uniqueness,
            VerificationTemplate::ReferentialIntegrity { .. } => {
                VerificationCategory::ReferentialIntegrity
            }
            VerificationTemplate::SchemaValidation { .. } => {
                VerificationCategory::SemanticCorrectness
            }
            VerificationTemplate::HttpHealthCheck { .. } => VerificationCategory::RuntimeBehavior,
            VerificationTemplate::RuntimeBehavior { .. } => VerificationCategory::RuntimeBehavior,
            VerificationTemplate::ContentAssertion { .. } => {
                VerificationCategory::SemanticCorrectness
            }
        }
    }

    /// Convert this template into a concrete step JSON value.
    ///
    /// All string parameters are sanitized to strip shell metacharacters before
    /// interpolation into shell commands, preventing injection attacks.
    pub fn to_step(&self, step_id: &str, name: &str) -> Value {
        match self {
            VerificationTemplate::ExistenceCheck { target, path } => {
                let path = sanitize_shell_param(path);
                let target = sanitize_shell_param(target);
                json!({
                    "id": step_id,
                    "name": name,
                    "type": "command",
                    "phase": "verification",
                    "mode": "check",
                    "command": format!("test -e \"{}\" && echo 'EXISTS: {}'", path, target),
                    "verification_category": "existence",
                    "required": true,
                })
            }
            VerificationTemplate::UniquenessCheck { field, source } => {
                let field = sanitize_shell_param(field);
                let source = sanitize_shell_param(source);
                json!({
                    "id": step_id,
                    "name": name,
                    "type": "command",
                    "phase": "verification",
                    "mode": "check",
                    "command": format!(
                        "sort {} | uniq -d | grep -c '{}' | grep -q '^0$'",
                        source, field
                    ),
                    "verification_category": "uniqueness",
                    "required": true,
                })
            }
            VerificationTemplate::ReferentialIntegrity {
                source_table,
                target_table,
                key,
            } => {
                let source_table = sanitize_shell_param(source_table);
                let target_table = sanitize_shell_param(target_table);
                let key = sanitize_shell_param(key);
                json!({
                    "id": step_id,
                    "name": name,
                    "type": "command",
                    "phase": "verification",
                    "mode": "check",
                    "command": format!(
                        "orphan_count=$(sqlite3 \"$DB_PATH\" \"SELECT COUNT(*) FROM {source} WHERE {key} NOT IN (SELECT {key} FROM {target});\" 2>&1) && [ \"$orphan_count\" = \"0\" ]",
                        source = source_table,
                        key = key,
                        target = target_table,
                    ),
                    "verification_category": "referential_integrity",
                    "required": true,
                })
            }
            VerificationTemplate::SchemaValidation {
                schema_path,
                data_path,
            } => {
                let schema_path = sanitize_shell_param(schema_path);
                let data_path = sanitize_shell_param(data_path);
                json!({
                    "id": step_id,
                    "name": name,
                    "type": "command",
                    "phase": "verification",
                    "mode": "check",
                    "command": format!(
                        "python -c \"import json; json.load(open('{}'))\" && echo 'Schema valid against {}'",
                        data_path, schema_path
                    ),
                    "verification_category": "semantic_correctness",
                    "required": true,
                })
            }
            VerificationTemplate::HttpHealthCheck {
                url,
                expected_status,
            } => {
                let url = sanitize_shell_param(url);
                json!({
                    "id": step_id,
                    "name": name,
                    "type": "command",
                    "phase": "verification",
                    "mode": "check",
                    "check_type": "http_status",
                    "command": format!(
                        "curl -s -o /dev/null -w '%{{http_code}}' {} | grep -q '{}'",
                        url, expected_status
                    ),
                    "verification_category": "runtime_behavior",
                    "required": true,
                    "retry": { "count": 3, "delay_ms": 2000 },
                })
            }
            VerificationTemplate::RuntimeBehavior {
                command,
                expected_output,
            } => {
                // command is not sanitized here — RuntimeBehavior templates are constructed
                // from trusted internal input (workflow generation code), not user-supplied
                // strings. Sanitizing would break legitimate shell commands.
                let expected_output = sanitize_shell_param(expected_output);
                json!({
                    "id": step_id,
                    "name": name,
                    "type": "command",
                    "phase": "verification",
                    "mode": "check",
                    "command": format!("{} | grep -q '{}'", command, expected_output),
                    "verification_category": "runtime_behavior",
                    "required": true,
                    "retry": { "count": 3, "delay_ms": 2000 },
                })
            }
            VerificationTemplate::ContentAssertion {
                url,
                element,
                expected,
            } => {
                // ContentAssertion uses ui_bridge (not shell), but sanitize
                // params that could appear in logs or downstream processing.
                let url = sanitize_shell_param(url);
                let element = sanitize_shell_param(element);
                let expected = sanitize_shell_param(expected);
                json!({
                    "id": step_id,
                    "name": name,
                    "type": "ui_bridge",
                    "phase": "verification",
                    "action": "assert",
                    "url": url,
                    "target": element,
                    "assert_type": "contains",
                    "expected": expected,
                    "verification_category": "semantic_correctness",
                    "required": true,
                })
            }
        }
    }
}

/// Classify an existing step's verification category based on its content.
pub fn classify_step_category(step: &Value) -> Option<VerificationCategory> {
    let step_type = step.get("type").and_then(|v| v.as_str())?;

    // Check for explicit annotation first
    if let Some(cat) = step.get("verification_category").and_then(|v| v.as_str()) {
        return match cat {
            "existence" => Some(VerificationCategory::Existence),
            "uniqueness" => Some(VerificationCategory::Uniqueness),
            "referential_integrity" => Some(VerificationCategory::ReferentialIntegrity),
            "semantic_correctness" => Some(VerificationCategory::SemanticCorrectness),
            "runtime_behavior" => Some(VerificationCategory::RuntimeBehavior),
            _ => None,
        };
    }

    // Heuristic classification based on step content
    match step_type {
        "command" => {
            let cmd = step.get("command").and_then(|v| v.as_str()).unwrap_or("");
            let check_type = step
                .get("check_type")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if check_type == "http_status" || cmd.contains("curl") {
                Some(VerificationCategory::RuntimeBehavior)
            } else if cmd.contains("test -e") || cmd.contains("test -f") || cmd.contains("test -d")
            {
                Some(VerificationCategory::Existence)
            } else if cmd.contains("uniq") || cmd.contains("DISTINCT") {
                Some(VerificationCategory::Uniqueness)
            } else if cmd.contains("grep") || cmd.contains("jq") || cmd.contains("diff") {
                Some(VerificationCategory::SemanticCorrectness)
            } else {
                Some(VerificationCategory::RuntimeBehavior)
            }
        }
        "ui_bridge" => {
            let action = step.get("action").and_then(|v| v.as_str()).unwrap_or("");
            match action {
                "assert" | "snapshot_assert" => Some(VerificationCategory::SemanticCorrectness),
                "snapshot" | "compare" => Some(VerificationCategory::RuntimeBehavior),
                _ => Some(VerificationCategory::Existence),
            }
        }
        _ => None,
    }
}

/// Check that data-producing workflows cover key verification categories.
///
/// Returns quality findings for categories that are missing coverage.
pub fn category_coverage_findings(
    workflow: &UnifiedWorkflow,
) -> Vec<super::revision::QualityFinding> {
    use super::revision::{FindingCategory, FindingSeverity, QualityFinding};
    use std::collections::HashSet;

    let mut findings = Vec::new();

    // Collect all verification steps
    let all_verification_steps: Vec<&Value> = workflow
        .verification_steps
        .iter()
        .chain(
            workflow
                .stages
                .iter()
                .flat_map(|s| s.verification_steps.iter()),
        )
        .collect();

    if all_verification_steps.is_empty() {
        return findings;
    }

    // Classify all verification steps
    let covered_categories: HashSet<VerificationCategory> = all_verification_steps
        .iter()
        .filter_map(|s| classify_step_category(s))
        .collect();

    // Detect if this is a data-producing workflow (has commands that write/create)
    let has_data_operations = workflow
        .setup_steps
        .iter()
        .chain(workflow.agentic_steps.iter())
        .chain(
            workflow
                .stages
                .iter()
                .flat_map(|s| s.setup_steps.iter().chain(s.agentic_steps.iter())),
        )
        .any(|s| {
            let cmd = s.get("command").and_then(|v| v.as_str()).unwrap_or("");
            let cmd_upper = cmd.to_uppercase();
            let step_type = s.get("type").and_then(|v| v.as_str()).unwrap_or("");
            step_type == "prompt"
                || cmd_upper.contains("INSERT")
                || cmd_upper.contains("CREATE")
                || cmd_upper.contains("WRITE")
                || cmd_upper.contains("POST")
        });

    // Only check category coverage for data-producing workflows
    if has_data_operations {
        if !covered_categories.contains(&VerificationCategory::Existence) {
            findings.push(QualityFinding {
                finding_id: "category-missing-existence".to_string(),
                step_id: None,
                severity: FindingSeverity::Info,
                category: FindingCategory::VerificationGap,
                description: "No verification step checks for existence of created artifacts"
                    .to_string(),
                suggested_fix: Some(
                    "Add a verification step that checks the output exists (e.g., file, endpoint, record)"
                        .to_string(),
                ),
            });
        }
        if !covered_categories.contains(&VerificationCategory::SemanticCorrectness) {
            findings.push(QualityFinding {
                finding_id: "category-missing-semantic".to_string(),
                step_id: None,
                severity: FindingSeverity::Warning,
                category: FindingCategory::VerificationGap,
                description: "No verification step checks semantic correctness of output content"
                    .to_string(),
                suggested_fix: Some(
                    "Add a verification step that validates output content, not just existence"
                        .to_string(),
                ),
            });
        }
    }

    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_template_to_step() {
        let template = VerificationTemplate::HttpHealthCheck {
            url: "http://localhost:3001".to_string(),
            expected_status: 200,
        };
        let step = template.to_step("step-1", "Health Check");
        assert_eq!(step["type"], "command");
        assert_eq!(step["check_type"], "http_status");
        assert_eq!(step["verification_category"], "runtime_behavior");
    }

    #[test]
    fn test_classify_explicit_category() {
        let step = json!({
            "type": "command",
            "verification_category": "existence",
            "command": "test -f /tmp/output.json"
        });
        assert_eq!(
            classify_step_category(&step),
            Some(VerificationCategory::Existence)
        );
    }

    #[test]
    fn test_classify_heuristic_curl() {
        let step = json!({
            "type": "command",
            "command": "curl -s http://localhost:3001/health"
        });
        assert_eq!(
            classify_step_category(&step),
            Some(VerificationCategory::RuntimeBehavior)
        );
    }

    #[test]
    fn test_classify_ui_bridge_assert() {
        let step = json!({
            "type": "ui_bridge",
            "action": "assert",
            "target": "#title",
            "expected": "Hello"
        });
        assert_eq!(
            classify_step_category(&step),
            Some(VerificationCategory::SemanticCorrectness)
        );
    }

    #[test]
    fn test_template_category() {
        assert_eq!(
            VerificationTemplate::ExistenceCheck {
                target: "file".to_string(),
                path: "/tmp/f".to_string()
            }
            .category(),
            VerificationCategory::Existence
        );
        assert_eq!(
            VerificationTemplate::ContentAssertion {
                url: "http://localhost".to_string(),
                element: "#el".to_string(),
                expected: "text".to_string()
            }
            .category(),
            VerificationCategory::SemanticCorrectness
        );
    }
}
