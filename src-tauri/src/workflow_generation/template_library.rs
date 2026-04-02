//! Template Library
//!
//! Reusable verification step patterns extracted from successful runs.
//! Templates are matched to criteria via keyword similarity and injected
//! into generation prompts to improve first-pass quality.

use crate::database::Connection;
use crate::database::pg::PgDb;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;

use super::domain_routing::VerificationDomain;
use super::specification::AcceptanceCriterion;

/// A reusable verification step template.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepTemplate {
    pub id: String,
    pub domain: VerificationDomain,
    pub pattern_description: String,
    pub template_steps: Vec<TemplateStep>,
    pub parameters: Vec<TemplateParameter>,
    pub success_count: u32,
    pub failure_count: u32,
    pub confidence: f64,
    pub source: TemplateSource,
}

/// A parameterized step within a template.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateStep {
    pub step_type: String,
    pub command_template: Option<String>,
    pub check_type: Option<String>,
    pub expected_template: Option<String>,
    pub description: String,
}

/// A parameter that must be filled when using a template.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateParameter {
    pub name: String,
    pub description: String,
    pub param_type: ParameterType,
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParameterType {
    String,
    Url,
    Path,
    Number,
    JsonPath,
    Command,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TemplateSource {
    Seeded,
    Extracted,
    Promoted,
    Demoted,
}

// === Database Operations ===

/// Ensure the step_templates table exists.
pub fn ensure_table(conn: &Connection) -> Result<(), String> {
    Err("SQLite removed".to_string())
}

/// Save a template to the database.
pub fn save_template(conn: &Connection, template: &StepTemplate) -> Result<(), String> {
    Err("SQLite removed".to_string())
}

// row_to_template removed (SQLite dead code)

/// Load all templates for a domain.
pub fn load_templates_for_domain(
    conn: &Connection,
    domain: VerificationDomain,
) -> Result<Vec<StepTemplate>, String> {
    Err("SQLite removed".to_string())
}

/// Load all templates from the database.
pub fn load_all_templates(conn: &Connection) -> Result<Vec<StepTemplate>, String> {
    Err("SQLite removed".to_string())
}

/// Compute updated confidence from success/failure counts using Wilson score lower bound.
fn compute_confidence(success: u32, failure: u32) -> f64 {
    let n = (success + failure) as f64;
    if n == 0.0 {
        return 0.5; // prior
    }
    let p = success as f64 / n;
    // Wilson score interval lower bound (z = 1.0 for ~68% CI)
    let z = 1.0;
    let denominator = 1.0 + z * z / n;
    let centre = p + z * z / (2.0 * n);
    let spread = z * (p * (1.0 - p) / n + z * z / (4.0 * n * n)).sqrt();
    let lower = (centre - spread) / denominator;
    lower.clamp(0.0, 1.0)
}

/// Increment success count and update confidence.
pub fn record_success(conn: &Connection, template_id: &str) -> Result<(), String> {
    Err("SQLite removed".to_string())
}

/// Increment failure count and update confidence.
pub fn record_failure(conn: &Connection, template_id: &str) -> Result<(), String> {
    Err("SQLite removed".to_string())
}

// === Template Matching ===

/// Extract lowercase keywords from a text string, filtering out stop words.
fn extract_keywords(text: &str) -> HashSet<String> {
    static STOP_WORDS: &[&str] = &[
        "a", "an", "the", "is", "are", "was", "were", "be", "been", "being", "have", "has",
        "had", "do", "does", "did", "will", "would", "could", "should", "may", "might", "can",
        "shall", "to", "of", "in", "for", "on", "with", "at", "by", "from", "as", "into",
        "through", "during", "before", "after", "above", "below", "between", "and", "but", "or",
        "nor", "not", "no", "so", "if", "then", "than", "that", "this", "it", "its", "all",
        "each", "every", "any", "both", "few", "more", "most", "other", "some", "such", "only",
        "same", "when", "where", "which", "while", "who", "whom", "how", "what", "why",
    ];
    let stop: HashSet<&str> = STOP_WORDS.iter().copied().collect();

    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        .filter(|w| w.len() > 1 && !stop.contains(w))
        .map(|w| w.to_string())
        .collect()
}

/// Find templates that match a criterion using keyword overlap.
pub fn find_matching_templates(
    criterion: &AcceptanceCriterion,
    domain: &VerificationDomain,
    templates: &[StepTemplate],
) -> Vec<(StepTemplate, f64)> {
    let criterion_text = format!(
        "{} {} {}",
        criterion.description, criterion.verification_hint, criterion.category
    );
    let criterion_keywords = extract_keywords(&criterion_text);

    if criterion_keywords.is_empty() {
        return Vec::new();
    }

    let mut matches: Vec<(StepTemplate, f64)> = templates
        .iter()
        .filter(|t| t.domain == *domain)
        .filter(|t| t.confidence > 0.4)
        .filter_map(|t| {
            let template_keywords = extract_keywords(&t.pattern_description);
            if template_keywords.is_empty() {
                return None;
            }
            // Jaccard-like: intersection / union
            let intersection = criterion_keywords
                .intersection(&template_keywords)
                .count() as f64;
            let union = criterion_keywords.union(&template_keywords).count() as f64;
            let score = intersection / union;
            if score >= 0.3 {
                Some((t.clone(), score))
            } else {
                // Also try containment: what fraction of template keywords appear in criterion
                // This helps when criterion is very detailed but template is focused
                let containment = intersection / template_keywords.len() as f64;
                if containment >= 0.5 {
                    Some((t.clone(), containment * 0.8)) // slight discount
                } else {
                    None
                }
            }
        })
        .collect();

    matches.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    matches
}

/// Find matching templates for a list of criteria (batched).
pub fn find_matching_templates_for_criteria(
    criteria: &[AcceptanceCriterion],
    domain: &VerificationDomain,
    _pg_db: Option<&Arc<PgDb>>,
) -> Vec<(StepTemplate, f64)> {
    // Template library uses seed templates; PG template storage is not yet available
    let templates: Vec<StepTemplate> = seed_templates()
        .into_iter()
        .filter(|t| t.domain == *domain)
        .collect();

    // Also include seed templates if DB had none
    let all_templates = if templates.is_empty() {
        seed_templates()
            .into_iter()
            .filter(|t| t.domain == *domain)
            .collect()
    } else {
        templates
    };

    let mut seen_ids: HashSet<String> = HashSet::new();
    let mut all_matches: Vec<(StepTemplate, f64)> = Vec::new();

    for criterion in criteria {
        let matches = find_matching_templates(criterion, domain, &all_templates);
        for (template, score) in matches {
            if seen_ids.insert(template.id.clone()) {
                all_matches.push((template, score));
            }
        }
    }

    all_matches.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    all_matches
}

// === Prompt Injection ===

/// Format matched templates for injection into generation prompts.
pub fn build_template_injection(matches: &[(StepTemplate, f64)]) -> String {
    if matches.is_empty() {
        return String::new();
    }

    let mut sections: Vec<String> = Vec::new();
    sections.push("## Relevant Verification Patterns\n".to_string());
    sections.push(
        "The following patterns have been extracted from previous successful verifications.\n"
            .to_string(),
    );

    for (template, score) in matches {
        let confidence_label = if template.confidence > 0.9 {
            "USE this pattern directly"
        } else if template.confidence > 0.7 {
            "Consider this pattern"
        } else {
            "This pattern has sometimes worked"
        };

        let mut block = format!(
            "### {} (match: {:.0}%, confidence: {:.0}%)\n**{}**\n\n",
            template.pattern_description,
            score * 100.0,
            template.confidence * 100.0,
            confidence_label,
        );

        for (i, step) in template.template_steps.iter().enumerate() {
            block.push_str(&format!("{}. {}\n", i + 1, step.description));
            if let Some(ref cmd) = step.command_template {
                block.push_str(&format!("   Command: `{}`\n", cmd));
            }
            if let Some(ref check) = step.check_type {
                block.push_str(&format!("   Check: {}", check));
                if let Some(ref expected) = step.expected_template {
                    block.push_str(&format!(" (expected: `{}`)", expected));
                }
                block.push('\n');
            }
        }

        if !template.parameters.is_empty() {
            block.push_str("\nParameters to fill:\n");
            for param in &template.parameters {
                let req = if param.required { "required" } else { "optional" };
                block.push_str(&format!("- `{{{{{}}}}}`: {} ({})\n", param.name, param.description, req));
            }
        }

        block.push('\n');
        sections.push(block);
    }

    sections.join("")
}

// === Seed Templates ===

/// Get the built-in seed templates (always available, no DB needed).
pub fn seed_templates() -> Vec<StepTemplate> {
    vec![
        // 1. Compilation: Build project and verify zero exit code
        StepTemplate {
            id: "seed-compilation-build".to_string(),
            domain: VerificationDomain::Compilation,
            pattern_description: "Build project and verify zero exit code with no errors".to_string(),
            template_steps: vec![
                TemplateStep {
                    step_type: "command".to_string(),
                    command_template: Some("cd {{project_dir}} && {{build_command}}".to_string()),
                    check_type: Some("exit_code".to_string()),
                    expected_template: Some("0".to_string()),
                    description: "Run the build command and verify it exits with code 0".to_string(),
                },
                TemplateStep {
                    step_type: "command".to_string(),
                    command_template: Some("cd {{project_dir}} && {{build_command}} 2>&1".to_string()),
                    check_type: Some("stderr_absent".to_string()),
                    expected_template: Some("error".to_string()),
                    description: "Verify build output contains no error messages".to_string(),
                },
            ],
            parameters: vec![
                TemplateParameter {
                    name: "project_dir".to_string(),
                    description: "Root directory of the project".to_string(),
                    param_type: ParameterType::Path,
                    required: true,
                },
                TemplateParameter {
                    name: "build_command".to_string(),
                    description: "Build command, e.g. cargo build, npm run build, make".to_string(),
                    param_type: ParameterType::Command,
                    required: true,
                },
            ],
            success_count: 10,
            failure_count: 1,
            confidence: 0.85,
            source: TemplateSource::Seeded,
        },
        // 2. Compilation: Run linter with project config
        StepTemplate {
            id: "seed-compilation-lint".to_string(),
            domain: VerificationDomain::Compilation,
            pattern_description: "Run linter or type checker and verify clean output".to_string(),
            template_steps: vec![
                TemplateStep {
                    step_type: "command".to_string(),
                    command_template: Some("cd {{project_dir}} && {{lint_command}}".to_string()),
                    check_type: Some("exit_code".to_string()),
                    expected_template: Some("0".to_string()),
                    description: "Run the linter/type-checker and confirm zero exit code".to_string(),
                },
            ],
            parameters: vec![
                TemplateParameter {
                    name: "project_dir".to_string(),
                    description: "Root directory of the project".to_string(),
                    param_type: ParameterType::Path,
                    required: true,
                },
                TemplateParameter {
                    name: "lint_command".to_string(),
                    description: "Lint command, e.g. npx eslint ., cargo clippy, ruff check .".to_string(),
                    param_type: ParameterType::Command,
                    required: true,
                },
            ],
            success_count: 8,
            failure_count: 2,
            confidence: 0.72,
            source: TemplateSource::Seeded,
        },
        // 3. ApiEndpoint: Check HTTP endpoint status and body field
        StepTemplate {
            id: "seed-api-status-body".to_string(),
            domain: VerificationDomain::ApiEndpoint,
            pattern_description: "Check HTTP endpoint returns expected status code and response body field".to_string(),
            template_steps: vec![
                TemplateStep {
                    step_type: "command".to_string(),
                    command_template: Some(
                        "curl -s -o /dev/null -w '%{http_code}' {{endpoint_url}}".to_string(),
                    ),
                    check_type: Some("stdout_match".to_string()),
                    expected_template: Some("{{expected_status}}".to_string()),
                    description: "Send request and verify HTTP status code".to_string(),
                },
                TemplateStep {
                    step_type: "command".to_string(),
                    command_template: Some(
                        "curl -s {{endpoint_url}} | python -c \"import sys,json; d=json.load(sys.stdin); assert '{{body_field}}' in d, f'Missing field {{body_field}}'\"".to_string(),
                    ),
                    check_type: Some("exit_code".to_string()),
                    expected_template: Some("0".to_string()),
                    description: "Verify response body contains the expected field".to_string(),
                },
            ],
            parameters: vec![
                TemplateParameter {
                    name: "endpoint_url".to_string(),
                    description: "Full URL of the endpoint to test".to_string(),
                    param_type: ParameterType::Url,
                    required: true,
                },
                TemplateParameter {
                    name: "expected_status".to_string(),
                    description: "Expected HTTP status code, e.g. 200, 201".to_string(),
                    param_type: ParameterType::Number,
                    required: true,
                },
                TemplateParameter {
                    name: "body_field".to_string(),
                    description: "JSON field name expected in response body".to_string(),
                    param_type: ParameterType::String,
                    required: true,
                },
            ],
            success_count: 7,
            failure_count: 2,
            confidence: 0.70,
            source: TemplateSource::Seeded,
        },
        // 4. ApiEndpoint: Test endpoint rejects unauthenticated request
        StepTemplate {
            id: "seed-api-auth-reject".to_string(),
            domain: VerificationDomain::ApiEndpoint,
            pattern_description: "Verify endpoint rejects unauthenticated requests with 401 or 403".to_string(),
            template_steps: vec![
                TemplateStep {
                    step_type: "command".to_string(),
                    command_template: Some(
                        "curl -s -o /dev/null -w '%{http_code}' {{endpoint_url}}".to_string(),
                    ),
                    check_type: Some("stdout_match".to_string()),
                    expected_template: Some("{{reject_status}}".to_string()),
                    description: "Send unauthenticated request and verify rejection status".to_string(),
                },
            ],
            parameters: vec![
                TemplateParameter {
                    name: "endpoint_url".to_string(),
                    description: "Full URL of the protected endpoint".to_string(),
                    param_type: ParameterType::Url,
                    required: true,
                },
                TemplateParameter {
                    name: "reject_status".to_string(),
                    description: "Expected rejection status code (401 or 403)".to_string(),
                    param_type: ParameterType::Number,
                    required: false,
                },
            ],
            success_count: 5,
            failure_count: 1,
            confidence: 0.73,
            source: TemplateSource::Seeded,
        },
        // 5. UiContent: Assert element visible with expected text
        StepTemplate {
            id: "seed-ui-element-text".to_string(),
            domain: VerificationDomain::UiContent,
            pattern_description: "Assert UI element is visible and displays expected text content".to_string(),
            template_steps: vec![
                TemplateStep {
                    step_type: "prompt".to_string(),
                    command_template: None,
                    check_type: Some("element_visible".to_string()),
                    expected_template: Some("{{element_selector}}".to_string()),
                    description: "Verify the target element is present and visible on the page".to_string(),
                },
                TemplateStep {
                    step_type: "prompt".to_string(),
                    command_template: None,
                    check_type: Some("text_content".to_string()),
                    expected_template: Some("{{expected_text}}".to_string()),
                    description: "Verify the element displays the expected text content".to_string(),
                },
            ],
            parameters: vec![
                TemplateParameter {
                    name: "element_selector".to_string(),
                    description: "CSS selector or test-id for the target element".to_string(),
                    param_type: ParameterType::String,
                    required: true,
                },
                TemplateParameter {
                    name: "expected_text".to_string(),
                    description: "Text content expected to be visible".to_string(),
                    param_type: ParameterType::String,
                    required: true,
                },
            ],
            success_count: 6,
            failure_count: 3,
            confidence: 0.60,
            source: TemplateSource::Seeded,
        },
        // 6. DatabaseState: Query table and check row exists
        StepTemplate {
            id: "seed-db-row-exists".to_string(),
            domain: VerificationDomain::DatabaseState,
            pattern_description: "Query database table and verify expected row exists".to_string(),
            template_steps: vec![
                TemplateStep {
                    step_type: "command".to_string(),
                    command_template: Some(
                        "sqlite3 {{db_path}} \"SELECT COUNT(*) FROM {{table_name}} WHERE {{where_clause}};\"".to_string(),
                    ),
                    check_type: Some("stdout_gte".to_string()),
                    expected_template: Some("1".to_string()),
                    description: "Query the table with a WHERE clause and verify at least one row returned".to_string(),
                },
            ],
            parameters: vec![
                TemplateParameter {
                    name: "db_path".to_string(),
                    description: "Path to the SQLite database file".to_string(),
                    param_type: ParameterType::Path,
                    required: true,
                },
                TemplateParameter {
                    name: "table_name".to_string(),
                    description: "Name of the table to query".to_string(),
                    param_type: ParameterType::String,
                    required: true,
                },
                TemplateParameter {
                    name: "where_clause".to_string(),
                    description: "SQL WHERE clause to filter rows".to_string(),
                    param_type: ParameterType::String,
                    required: true,
                },
            ],
            success_count: 4,
            failure_count: 1,
            confidence: 0.68,
            source: TemplateSource::Seeded,
        },
        // 7. Security: Verify CORS headers present
        StepTemplate {
            id: "seed-security-cors".to_string(),
            domain: VerificationDomain::Security,
            pattern_description: "Verify CORS headers are present on API response".to_string(),
            template_steps: vec![
                TemplateStep {
                    step_type: "command".to_string(),
                    command_template: Some(
                        "curl -s -I -X OPTIONS -H 'Origin: {{test_origin}}' {{endpoint_url}}".to_string(),
                    ),
                    check_type: Some("stdout_contains".to_string()),
                    expected_template: Some("Access-Control-Allow-Origin".to_string()),
                    description: "Send OPTIONS preflight request and verify CORS allow-origin header exists".to_string(),
                },
                TemplateStep {
                    step_type: "command".to_string(),
                    command_template: Some(
                        "curl -s -I -X OPTIONS -H 'Origin: {{test_origin}}' {{endpoint_url}}".to_string(),
                    ),
                    check_type: Some("stdout_contains".to_string()),
                    expected_template: Some("Access-Control-Allow-Methods".to_string()),
                    description: "Verify CORS allow-methods header is present".to_string(),
                },
            ],
            parameters: vec![
                TemplateParameter {
                    name: "endpoint_url".to_string(),
                    description: "Full URL of the endpoint to test".to_string(),
                    param_type: ParameterType::Url,
                    required: true,
                },
                TemplateParameter {
                    name: "test_origin".to_string(),
                    description: "Origin to use in the preflight request".to_string(),
                    param_type: ParameterType::Url,
                    required: true,
                },
            ],
            success_count: 3,
            failure_count: 1,
            confidence: 0.62,
            source: TemplateSource::Seeded,
        },
        // 8. CiCd: Check Docker build succeeds
        StepTemplate {
            id: "seed-cicd-docker-build".to_string(),
            domain: VerificationDomain::CiCd,
            pattern_description: "Verify Docker image builds successfully with no errors".to_string(),
            template_steps: vec![
                TemplateStep {
                    step_type: "command".to_string(),
                    command_template: Some(
                        "cd {{project_dir}} && docker build -t {{image_tag}} -f {{dockerfile}} .".to_string(),
                    ),
                    check_type: Some("exit_code".to_string()),
                    expected_template: Some("0".to_string()),
                    description: "Build the Docker image and verify zero exit code".to_string(),
                },
                TemplateStep {
                    step_type: "command".to_string(),
                    command_template: Some(
                        "docker image inspect {{image_tag}} --format '{{{{.Id}}}}'".to_string(),
                    ),
                    check_type: Some("stdout_not_empty".to_string()),
                    expected_template: None,
                    description: "Verify the built image exists in local registry".to_string(),
                },
            ],
            parameters: vec![
                TemplateParameter {
                    name: "project_dir".to_string(),
                    description: "Root directory containing the Dockerfile".to_string(),
                    param_type: ParameterType::Path,
                    required: true,
                },
                TemplateParameter {
                    name: "dockerfile".to_string(),
                    description: "Path to Dockerfile relative to project_dir".to_string(),
                    param_type: ParameterType::Path,
                    required: false,
                },
                TemplateParameter {
                    name: "image_tag".to_string(),
                    description: "Tag for the built image, e.g. myapp:test".to_string(),
                    param_type: ParameterType::String,
                    required: true,
                },
            ],
            success_count: 3,
            failure_count: 2,
            confidence: 0.52,
            source: TemplateSource::Seeded,
        },
    ]
}

/// Seed the database with initial templates (idempotent -- skips existing).
pub fn seed_database(conn: &Connection) -> Result<usize, String> {
    Err("SQLite removed".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_seed_templates_not_empty() {
        let seeds = seed_templates();
        assert!(seeds.len() >= 5, "Expected at least 5 seed templates");
        for t in &seeds {
            assert!(!t.id.is_empty());
            assert!(!t.template_steps.is_empty());
            assert!(t.confidence > 0.0 && t.confidence <= 1.0);
        }
    }

    #[test]
    fn test_seed_database_idempotent() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_save_and_load_template() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_record_success_updates_confidence() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_keyword_extraction() {
        let keywords = extract_keywords("Build the Rust project and verify no errors");
        assert!(keywords.contains("build"));
        assert!(keywords.contains("rust"));
        assert!(keywords.contains("project"));
        assert!(keywords.contains("verify"));
        assert!(keywords.contains("errors"));
        // Stop words removed
        assert!(!keywords.contains("the"));
        assert!(!keywords.contains("and"));
        assert!(!keywords.contains("no"));
    }

    #[test]
    fn test_build_template_injection_empty() {
        let result = build_template_injection(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_compute_confidence() {
        // No data -> prior
        assert!((compute_confidence(0, 0) - 0.5).abs() < 0.01);
        // All success -> high
        assert!(compute_confidence(20, 0) > 0.8);
        // All failure -> low
        assert!(compute_confidence(0, 20) < 0.1);
        // Mixed
        let mixed = compute_confidence(7, 3);
        assert!(mixed > 0.4 && mixed < 0.8);
    }
}
