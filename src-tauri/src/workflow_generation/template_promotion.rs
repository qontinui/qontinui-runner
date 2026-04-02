//! Dynamic template promotion.
//!
//! Automatically promotes high-quality AI-generated workflows into
//! parameterized templates that can be reused for similar tasks.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::time::Instant;
use tracing::{debug, info, warn};
use uuid::Uuid;

// ============================================================================
// Types
// ============================================================================

/// A promoted template derived from a successful workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromotedTemplate {
    /// Template ID (generated).
    pub id: String,
    /// Template name (derived from workflow name).
    pub name: String,
    /// Template description.
    pub description: String,
    /// The parameterized workflow JSON (with {{variable}} placeholders).
    pub template_json: serde_json::Value,
    /// Parameters extracted from the workflow.
    pub parameters: Vec<TemplateParameter>,
    /// Quality score of the source workflow.
    pub source_quality_score: f32,
    /// Number of successful executions of the source workflow.
    pub success_count: u32,
    /// Source workflow ID.
    pub source_workflow_id: String,
    /// When this template was created.
    pub created_at: String,
    /// Category tags.
    pub tags: Vec<String>,
}

/// A parameter extracted from a workflow for template parameterization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateParameter {
    /// Parameter name (e.g., "app_url", "project_name").
    pub name: String,
    /// Description of what this parameter represents.
    pub description: String,
    /// Default value from the source workflow.
    pub default_value: String,
    /// The pattern used to identify this as a variable part.
    pub extraction_pattern: String,
}

/// Result of a template promotion run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromotionResult {
    pub promoted: Vec<PromotedTemplate>,
    pub candidates_evaluated: u32,
    pub duration_ms: u64,
}

// ============================================================================
// Storage
// ============================================================================

/// Ensure the promoted_templates table exists.
fn ensure_table() -> Result<(), String> {
    Err("SQLite removed".to_string())
}

// ============================================================================
// Public API
// ============================================================================

/// Evaluate workflows for template promotion and promote qualifying ones.
pub fn evaluate_and_promote() -> Result<PromotionResult, String> {
    Err("SQLite removed".to_string())
}

/// Find promoted templates relevant to a description.
pub fn find_relevant_templates(
    description: &str,
) -> Result<Vec<PromotedTemplate>, String> {
    Err("SQLite removed".to_string())
}

/// Format promoted templates for the builder prompt.
pub fn format_templates_for_prompt(templates: &[PromotedTemplate]) -> String {
    if templates.is_empty() {
        return String::new();
    }

    let mut output = String::from("## Reusable Workflow Templates\n\n");
    output.push_str(
        "The following templates were derived from proven workflows. \
         You may use them as a starting point and fill in the parameters.\n\n",
    );

    for (i, t) in templates.iter().enumerate() {
        output.push_str(&format!(
            "### Template {}: {} ({} successful runs)\n\n",
            i + 1,
            t.name,
            t.success_count,
        ));
        output.push_str(&format!("**Description**: {}\n\n", t.description));

        if !t.parameters.is_empty() {
            output.push_str("**Parameters**:\n");
            for p in &t.parameters {
                output.push_str(&format!(
                    "- `{{{{{}}}}}` — {} (default: `{}`)\n",
                    p.name, p.description, p.default_value,
                ));
            }
            output.push('\n');
        }

        if let Ok(json_str) = serde_json::to_string_pretty(&t.template_json) {
            // Truncate very large templates to keep prompt manageable
            if json_str.len() < 2000 {
                output.push_str("**Template JSON**:\n```json\n");
                output.push_str(&json_str);
                output.push_str("\n```\n\n");
            } else {
                output.push_str("_(Template JSON omitted for brevity — too large)_\n\n");
            }
        }
    }

    output
}

// ============================================================================
// Parameter extraction
// ============================================================================

/// Extract parameterizable parts from a workflow.
fn extract_parameters(workflow_json: &serde_json::Value) -> Vec<TemplateParameter> {
    let mut params: Vec<TemplateParameter> = Vec::new();
    let mut all_strings: Vec<String> = Vec::new();

    // Collect all string values from the JSON
    collect_strings(workflow_json, &mut all_strings);

    // Deduplicate for frequency analysis
    let mut string_counts: std::collections::HashMap<String, u32> =
        std::collections::HashMap::new();
    for s in &all_strings {
        *string_counts.entry(s.clone()).or_insert(0) += 1;
    }

    let mut seen_params: std::collections::HashSet<String> = std::collections::HashSet::new();

    for s in &all_strings {
        // URL patterns (host:port or http(s)://...)
        if !seen_params.contains("app_url") && is_url_pattern(s) {
            seen_params.insert("app_url".to_string());
            params.push(TemplateParameter {
                name: "app_url".to_string(),
                description: "Application URL or host:port".to_string(),
                default_value: s.clone(),
                extraction_pattern: r"https?://[^\s/]+(?::\d+)?|localhost:\d+".to_string(),
            });
        }

        // File path patterns
        if !seen_params.contains("project_path") && is_file_path(s) {
            seen_params.insert("project_path".to_string());
            params.push(TemplateParameter {
                name: "project_path".to_string(),
                description: "Project directory path".to_string(),
                default_value: s.clone(),
                extraction_pattern: r"(?:/[\w.-]+){2,}|(?:[A-Z]:\\[\w.-]+(?:\\[\w.-]+)*)"
                    .to_string(),
            });
        }

        // Port numbers
        if !seen_params.contains("port") && is_port_number(s) {
            seen_params.insert("port".to_string());
            params.push(TemplateParameter {
                name: "port".to_string(),
                description: "Service port number".to_string(),
                default_value: s.clone(),
                extraction_pattern: r"\b\d{4,5}\b".to_string(),
            });
        }
    }

    // Project-specific names: strings that appear 3+ times and look like identifiers
    for (value, count) in &string_counts {
        if *count >= 3
            && !seen_params.contains("project_name")
            && is_identifier_like(value)
            && value.len() >= 3
            && value.len() <= 50
        {
            seen_params.insert("project_name".to_string());
            params.push(TemplateParameter {
                name: "project_name".to_string(),
                description: "Project-specific name that appears across the workflow".to_string(),
                default_value: value.clone(),
                extraction_pattern: format!(r"\b{}\b", regex_escape(value)),
            });
        }
    }

    params
}

/// Recursively collect all string values from a JSON value.
fn collect_strings(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(s) => {
            if !s.is_empty() {
                out.push(s.clone());
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                collect_strings(item, out);
            }
        }
        serde_json::Value::Object(map) => {
            for (_, v) in map {
                collect_strings(v, out);
            }
        }
        _ => {}
    }
}

/// Check if a string looks like a URL or host:port pattern.
fn is_url_pattern(s: &str) -> bool {
    s.starts_with("http://")
        || s.starts_with("https://")
        || (s.contains("localhost:") && s.len() < 100)
        || (s.contains("127.0.0.1:") && s.len() < 100)
}

/// Check if a string looks like a file path.
fn is_file_path(s: &str) -> bool {
    let trimmed = s.trim();
    // Unix-style paths
    if trimmed.starts_with('/') && trimmed.len() > 3 && trimmed.contains('/') {
        // Exclude URLs
        if !trimmed.starts_with("http") {
            return true;
        }
    }
    // Windows-style paths
    if trimmed.len() > 3
        && trimmed.chars().nth(1) == Some(':')
        && trimmed.chars().nth(2) == Some('\\')
    {
        return true;
    }
    false
}

/// Check if a string looks like a standalone port number.
fn is_port_number(s: &str) -> bool {
    if let Ok(n) = s.parse::<u32>() {
        (1024..=65535).contains(&n)
    } else {
        false
    }
}

/// Check if a string looks like a project/identifier name.
fn is_identifier_like(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 50
        && !s.contains(' ')
        && !s.starts_with('/')
        && !s.starts_with("http")
        && s.chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.')
}

/// Simple regex escape helper (avoids pulling in the regex crate just for this).
fn regex_escape(s: &str) -> String {
    let special = r"\.+*?()|[]{}^$";
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if special.contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Replace extracted parameter values in the workflow JSON with {{placeholder}} markers.
fn parameterize_json(
    workflow_json: &serde_json::Value,
    params: &[TemplateParameter],
) -> serde_json::Value {
    let mut json_str = serde_json::to_string(workflow_json).unwrap_or_default();

    for p in params {
        if !p.default_value.is_empty() {
            json_str = json_str.replace(&p.default_value, &format!("{{{{{}}}}}", p.name));
        }
    }

    serde_json::from_str(&json_str).unwrap_or_else(|_| workflow_json.clone())
}

/// Extract category tags from a workflow for search matching.
fn extract_tags(workflow_json: &serde_json::Value, description: &str) -> Vec<String> {
    let mut tags: Vec<String> = Vec::new();

    // Extract category from workflow data
    if let Some(cat) = workflow_json.get("category").and_then(|v| v.as_str()) {
        tags.push(cat.to_string());
    }

    // Extract technology keywords from description
    let tech_keywords = [
        "react",
        "vue",
        "angular",
        "node",
        "python",
        "rust",
        "java",
        "go",
        "docker",
        "kubernetes",
        "api",
        "web",
        "mobile",
        "database",
        "frontend",
        "backend",
        "fullstack",
        "microservice",
        "serverless",
        "ci",
        "cd",
        "test",
        "lint",
        "format",
        "typecheck",
        "deploy",
    ];
    let desc_lower = description.to_lowercase();
    for kw in &tech_keywords {
        if desc_lower.contains(kw) {
            tags.push(kw.to_string());
        }
    }

    tags.dedup();
    tags
}

/// Store a promoted template in the database.
fn store_template(template: &PromotedTemplate) -> Result<(), String> {
    Err("SQLite removed".to_string())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_url_pattern() {
        assert!(is_url_pattern("http://localhost:3000"));
        assert!(is_url_pattern("https://example.com/api"));
        assert!(is_url_pattern("localhost:8080"));
        assert!(!is_url_pattern("hello world"));
        assert!(!is_url_pattern("cargo test"));
    }

    #[test]
    fn test_is_file_path() {
        assert!(is_file_path("/home/user/project"));
        assert!(is_file_path("/usr/local/bin"));
        assert!(!is_file_path("hello"));
        assert!(!is_file_path("http://example.com"));
    }

    #[test]
    fn test_is_port_number() {
        assert!(is_port_number("3000"));
        assert!(is_port_number("8080"));
        assert!(!is_port_number("80")); // Below 1024
        assert!(!is_port_number("abc"));
        assert!(!is_port_number("99999")); // Above 65535
    }

    #[test]
    fn test_is_identifier_like() {
        assert!(is_identifier_like("my-project"));
        assert!(is_identifier_like("app_name"));
        assert!(!is_identifier_like("has space"));
        assert!(!is_identifier_like("/path/to"));
        assert!(!is_identifier_like("http://url"));
        assert!(!is_identifier_like("")); // empty
    }

    #[test]
    fn test_extract_parameters_with_url() {
        let json = serde_json::json!({
            "steps": [
                {"command": "curl http://localhost:3000/health"},
                {"url": "http://localhost:3000/api"},
            ]
        });
        let params = extract_parameters(&json);
        assert!(params.iter().any(|p| p.name == "app_url"));
    }

    #[test]
    fn test_extract_parameters_with_path() {
        let json = serde_json::json!({
            "config": {
                "dir": "/home/user/my-project",
                "output": "/tmp/results"
            }
        });
        let params = extract_parameters(&json);
        assert!(params.iter().any(|p| p.name == "project_path"));
    }

    #[test]
    fn test_parameterize_json() {
        let json = serde_json::json!({
            "url": "http://localhost:3000",
            "check": "http://localhost:3000/health"
        });
        let params = vec![TemplateParameter {
            name: "app_url".to_string(),
            description: "App URL".to_string(),
            default_value: "http://localhost:3000".to_string(),
            extraction_pattern: "".to_string(),
        }];
        let result = parameterize_json(&json, &params);
        let result_str = serde_json::to_string(&result).unwrap();
        assert!(result_str.contains("{{app_url}}"));
        assert!(!result_str.contains("http://localhost:3000"));
    }

    #[test]
    fn test_format_templates_for_prompt_empty() {
        let result = format_templates_for_prompt(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_format_templates_for_prompt_with_data() {
        let templates = vec![PromotedTemplate {
            id: "tmpl-1".to_string(),
            name: "Template: My Workflow".to_string(),
            description: "A test workflow".to_string(),
            template_json: serde_json::json!({"steps": []}),
            parameters: vec![TemplateParameter {
                name: "app_url".to_string(),
                description: "Application URL".to_string(),
                default_value: "http://localhost:3000".to_string(),
                extraction_pattern: "".to_string(),
            }],
            source_quality_score: 0.9,
            success_count: 5,
            source_workflow_id: "wf-1".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            tags: vec!["test".to_string()],
        }];
        let result = format_templates_for_prompt(&templates);
        assert!(result.contains("Reusable Workflow Templates"));
        assert!(result.contains("Template: My Workflow"));
        assert!(result.contains("{{app_url}}"));
        assert!(result.contains("5 successful runs"));
    }

    #[test]
    fn test_extract_tags() {
        let json = serde_json::json!({"category": "testing"});
        let tags = extract_tags(&json, "Run python linting with docker");
        assert!(tags.contains(&"testing".to_string()));
        assert!(tags.contains(&"python".to_string()));
        assert!(tags.contains(&"docker".to_string()));
        assert!(tags.contains(&"lint".to_string()));
    }

    #[test]
    fn test_regex_escape() {
        assert_eq!(regex_escape("hello.world"), r"hello\.world");
        assert_eq!(regex_escape("a+b"), r"a\+b");
        assert_eq!(regex_escape("simple"), "simple");
    }

    #[test]
    fn test_collect_strings() {
        let json = serde_json::json!({
            "name": "test",
            "items": ["a", "b"],
            "nested": {"key": "value"},
            "number": 42
        });
        let mut strings = Vec::new();
        collect_strings(&json, &mut strings);
        assert!(strings.contains(&"test".to_string()));
        assert!(strings.contains(&"a".to_string()));
        assert!(strings.contains(&"b".to_string()));
        assert!(strings.contains(&"value".to_string()));
        assert_eq!(strings.len(), 4); // number is not collected
    }
}
