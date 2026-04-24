//! Domain Classification & Phase Planning
//!
//! Classifies acceptance criteria into verification domains using keyword
//! matching, and plans generation phases with dependency ordering.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::debug;

use super::specification::AcceptanceCriterion;

/// Verification domains for specialized generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationDomain {
    /// Build, lint, typecheck, format
    Compilation,
    /// HTTP endpoints, status codes, response bodies
    ApiEndpoint,
    /// DOM elements, visual state, user interactions
    UiContent,
    /// Schema, data integrity, migrations
    DatabaseState,
    /// Auth, permissions, input validation, CORS
    Security,
    /// Response times, resource usage, load
    Performance,
    /// Service-to-service, external dependencies
    Integration,
    /// Pipeline, deployment, artifacts
    CiCd,
    /// Fallback for uncategorized criteria
    General,
}

impl VerificationDomain {
    /// All specific domains (excluding General).
    pub fn all_specific() -> &'static [VerificationDomain] {
        &[
            VerificationDomain::Compilation,
            VerificationDomain::ApiEndpoint,
            VerificationDomain::UiContent,
            VerificationDomain::DatabaseState,
            VerificationDomain::Security,
            VerificationDomain::Performance,
            VerificationDomain::Integration,
            VerificationDomain::CiCd,
        ]
    }

    /// Canonical execution order (dependencies flow left to right).
    pub fn dependency_order() -> &'static [VerificationDomain] {
        &[
            VerificationDomain::Compilation,
            VerificationDomain::DatabaseState,
            VerificationDomain::ApiEndpoint,
            VerificationDomain::Security,
            VerificationDomain::UiContent,
            VerificationDomain::Performance,
            VerificationDomain::Integration,
            VerificationDomain::CiCd,
        ]
    }

    /// Get domains this domain depends on.
    pub fn dependencies(&self) -> Vec<VerificationDomain> {
        match self {
            VerificationDomain::Compilation => vec![],
            VerificationDomain::DatabaseState => vec![],
            VerificationDomain::ApiEndpoint => vec![
                VerificationDomain::Compilation,
                VerificationDomain::DatabaseState,
            ],
            VerificationDomain::Security => vec![VerificationDomain::ApiEndpoint],
            VerificationDomain::UiContent => vec![VerificationDomain::ApiEndpoint],
            VerificationDomain::Performance => vec![VerificationDomain::ApiEndpoint],
            VerificationDomain::Integration => vec![VerificationDomain::ApiEndpoint],
            VerificationDomain::CiCd => vec![
                VerificationDomain::Compilation,
                VerificationDomain::Integration,
            ],
            VerificationDomain::General => vec![],
        }
    }

    /// Keywords that map to this domain (for classification).
    fn keywords(&self) -> &'static [&'static str] {
        match self {
            VerificationDomain::Compilation => &[
                "compile",
                "build",
                "lint",
                "typecheck",
                "tsc",
                "eslint",
                "format",
                "cargo",
                "rustc",
                "gcc",
                "make",
                "webpack",
                "vite",
            ],
            VerificationDomain::ApiEndpoint => &[
                "endpoint",
                "api",
                "status",
                "response",
                "curl",
                "http",
                "rest",
                "graphql",
                "fetch",
                "route",
                "handler",
                "middleware",
            ],
            VerificationDomain::UiContent => &[
                "button",
                "page",
                "element",
                "click",
                "display",
                "visible",
                "render",
                "component",
                "dom",
                "css",
                "style",
                "layout",
                "text",
                "ui bridge",
            ],
            VerificationDomain::DatabaseState => &[
                "table",
                "column",
                "migration",
                "query",
                "database",
                "sql",
                "schema",
                "index",
                "constraint",
                "row",
                "insert",
                "select",
            ],
            VerificationDomain::Security => &[
                "auth",
                "permission",
                "token",
                "cors",
                "sanitize",
                "csrf",
                "xss",
                "injection",
                "encrypt",
                "password",
                "session",
                "jwt",
                "rbac",
            ],
            VerificationDomain::Performance => &[
                "latency",
                "throughput",
                "load",
                "benchmark",
                "memory",
                "cpu",
                "cache",
                "timeout",
                "response time",
                "concurrent",
            ],
            VerificationDomain::Integration => &[
                "service",
                "webhook",
                "queue",
                "message",
                "pubsub",
                "grpc",
                "socket",
                "event",
                "upstream",
                "downstream",
            ],
            VerificationDomain::CiCd => &[
                "pipeline",
                "deploy",
                "artifact",
                "docker",
                "ci",
                "cd",
                "github actions",
                "workflow",
                "release",
                "container",
                "registry",
            ],
            VerificationDomain::General => &[],
        }
    }
}

impl std::fmt::Display for VerificationDomain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

/// Classify criteria into domains using keyword matching.
///
/// Each criterion is assigned to the domain with the most keyword matches
/// in its description, verification_hint, and category fields.
/// Falls back to General if no keywords match.
pub fn classify_criteria_domains(
    criteria: &[AcceptanceCriterion],
) -> HashMap<VerificationDomain, Vec<String>> {
    let mut result: HashMap<VerificationDomain, Vec<String>> = HashMap::new();

    for criterion in criteria {
        let searchable = format!(
            "{} {} {}",
            criterion.description, criterion.verification_hint, criterion.category
        )
        .to_lowercase();

        // Check direct category mapping first
        let category_domain = category_to_domain(&criterion.category);

        let mut best_domain = VerificationDomain::General;
        let mut best_count: usize = 0;

        for domain in VerificationDomain::all_specific() {
            let mut count: usize = 0;
            for keyword in domain.keywords() {
                if searchable.contains(keyword) {
                    count += 1;
                }
            }

            // Give a bonus if the category directly maps to this domain
            if let Some(cat_domain) = category_domain {
                if cat_domain == *domain {
                    count += 3;
                }
            }

            if count > best_count {
                best_count = count;
                best_domain = *domain;
            }
        }

        debug!(
            criterion_id = %criterion.id,
            domain = %best_domain,
            match_count = best_count,
            "Classified criterion into domain"
        );

        result
            .entry(best_domain)
            .or_default()
            .push(criterion.id.clone());
    }

    result
}

/// Category string to domain mapping (for direct category matches).
fn category_to_domain(category: &str) -> Option<VerificationDomain> {
    match category.to_lowercase().as_str() {
        "compilation" | "build" => Some(VerificationDomain::Compilation),
        "api" | "endpoint" | "api-endpoint" => Some(VerificationDomain::ApiEndpoint),
        "ui" | "ui-content" | "frontend" => Some(VerificationDomain::UiContent),
        "database" | "db" | "data" => Some(VerificationDomain::DatabaseState),
        "security" | "auth" | "authentication" => Some(VerificationDomain::Security),
        "performance" | "perf" => Some(VerificationDomain::Performance),
        "integration" | "e2e" => Some(VerificationDomain::Integration),
        "ci" | "cd" | "cicd" | "ci-cd" | "deployment" => Some(VerificationDomain::CiCd),
        _ => None,
    }
}

/// Classify criteria with LLM fallback for ambiguous cases.
///
/// First runs keyword classification. For criteria that fall to `General`
/// (no keyword matches), batches them into a single LLM call to get
/// domain assignments. Falls back to `General` if the LLM call fails.
pub fn classify_criteria_domains_with_llm_fallback(
    criteria: &[AcceptanceCriterion],
    doctor_handle: Option<&crate::doctor::DoctorHandle>,
    model_override: Option<&str>,
    provider_override: Option<&str>,
) -> HashMap<VerificationDomain, Vec<String>> {
    let mut result = classify_criteria_domains(criteria);

    // Check if any criteria fell to General
    let general_ids: Vec<String> = result
        .get(&VerificationDomain::General)
        .cloned()
        .unwrap_or_default();

    if general_ids.is_empty() {
        return result;
    }

    // Find the actual criteria objects for General-classified ones
    let general_criteria: Vec<&AcceptanceCriterion> = criteria
        .iter()
        .filter(|c| general_ids.contains(&c.id))
        .collect();

    debug!(
        count = general_criteria.len(),
        "Running LLM fallback for unclassified criteria"
    );

    // Build classification prompt
    let domains_list = VerificationDomain::all_specific()
        .iter()
        .map(|d| format!("- {}: {}", d, domain_description(d)))
        .collect::<Vec<_>>()
        .join("\n");

    let criteria_list = general_criteria
        .iter()
        .map(|c| {
            format!(
                "- id=\"{}\" description=\"{}\" hint=\"{}\" category=\"{}\"",
                c.id, c.description, c.verification_hint, c.category
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let prompt = format!(
        "You are a verification domain classifier. Classify each criterion into exactly one domain.\n\n\
        Available domains:\n{}\n\n\
        Criteria to classify:\n{}\n\n\
        Respond with ONLY a JSON object mapping criterion IDs to domain names. Example:\n\
        {{\"criterion-1\": \"compilation\", \"criterion-2\": \"api_endpoint\"}}\n\n\
        Valid domain names: compilation, api_endpoint, ui_content, database_state, security, performance, integration, ci_cd",
        domains_list, criteria_list
    );

    let task_context = crate::ai_router::TaskContext::from_prompt(&prompt);
    let ai_response = crate::ai_provider::run_prompt_with_model_override(
        &prompt,
        &task_context,
        doctor_handle,
        model_override,
        provider_override,
        Some(0.0), // temperature — deterministic classification
        None,      // max_tokens
        None,      // fallback_model
        None,      // fallback_provider
    );

    if !ai_response.success {
        debug!("LLM domain classification failed, keeping General assignments");
        return result;
    }

    // Parse the LLM response as JSON
    match parse_llm_classification(&ai_response.output) {
        Some(classifications) => {
            // Remove reclassified criteria from General
            if let Some(general_list) = result.get_mut(&VerificationDomain::General) {
                general_list.retain(|id| !classifications.contains_key(id));
                if general_list.is_empty() {
                    result.remove(&VerificationDomain::General);
                }
            }

            // Add to their new domains
            for (criterion_id, domain) in classifications {
                result.entry(domain).or_default().push(criterion_id);
            }
        }
        None => {
            debug!("Failed to parse LLM classification response");
        }
    }

    result
}

/// Parse LLM classification response into domain assignments.
fn parse_llm_classification(output: &str) -> Option<HashMap<String, VerificationDomain>> {
    let json_str = super::generator::extract_json_from_response(output);
    let parsed: serde_json::Value = serde_json::from_str(&json_str).ok()?;
    let obj = parsed.as_object()?;

    let mut result = HashMap::new();
    for (key, value) in obj {
        if let Some(domain_str) = value.as_str() {
            if let Some(domain) = str_to_domain(domain_str) {
                result.insert(key.clone(), domain);
            }
        }
    }

    Some(result)
}

/// Convert a domain name string to VerificationDomain.
fn str_to_domain(s: &str) -> Option<VerificationDomain> {
    match s.to_lowercase().as_str() {
        "compilation" => Some(VerificationDomain::Compilation),
        "api_endpoint" | "apiendpoint" => Some(VerificationDomain::ApiEndpoint),
        "ui_content" | "uicontent" => Some(VerificationDomain::UiContent),
        "database_state" | "databasestate" => Some(VerificationDomain::DatabaseState),
        "security" => Some(VerificationDomain::Security),
        "performance" => Some(VerificationDomain::Performance),
        "integration" => Some(VerificationDomain::Integration),
        "ci_cd" | "cicd" => Some(VerificationDomain::CiCd),
        _ => None,
    }
}

/// Brief description of each domain for the LLM prompt.
fn domain_description(domain: &VerificationDomain) -> &'static str {
    match domain {
        VerificationDomain::Compilation => "Build, lint, typecheck, formatting checks",
        VerificationDomain::ApiEndpoint => {
            "HTTP endpoints, status codes, request/response verification"
        }
        VerificationDomain::UiContent => "DOM elements, visual state, user interaction, rendering",
        VerificationDomain::DatabaseState => "Schema, data integrity, migrations, queries",
        VerificationDomain::Security => {
            "Authentication, permissions, input validation, CORS, tokens"
        }
        VerificationDomain::Performance => "Latency, throughput, resource usage, caching",
        VerificationDomain::Integration => "Service-to-service communication, webhooks, queues",
        VerificationDomain::CiCd => "CI/CD pipelines, deployments, Docker, artifacts",
        VerificationDomain::General => "General verification (fallback)",
    }
}

#[cfg(test)]
mod tests {
    use super::super::specification::{AcceptanceCriterion, CriterionPriority, VerificationMethod};
    use super::*;
    use std::collections::HashSet;

    fn make_criterion(id: &str, desc: &str, hint: &str, category: &str) -> AcceptanceCriterion {
        AcceptanceCriterion {
            id: id.to_string(),
            description: desc.to_string(),
            method: VerificationMethod::Command,
            priority: CriterionPriority::Critical,
            verification_hint: hint.to_string(),
            category: category.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn test_classify_compilation_criteria() {
        let criteria = vec![
            make_criterion("c1", "build succeeds", "run build", "general"),
            make_criterion("c2", "compile the project", "compile check", "general"),
            make_criterion("c3", "typecheck passes", "tsc --noEmit", "general"),
        ];
        let result = classify_criteria_domains(&criteria);
        assert_eq!(
            result
                .get(&VerificationDomain::Compilation)
                .map(|v| v.len()),
            Some(3),
            "All three criteria should be classified as Compilation"
        );
    }

    #[test]
    fn test_classify_api_criteria() {
        let criteria = vec![
            make_criterion("a1", "endpoint returns 200", "curl /api/health", "general"),
            make_criterion("a2", "curl the service", "check HTTP response", "general"),
            make_criterion("a3", "HTTP status is 201", "verify endpoint", "general"),
        ];
        let result = classify_criteria_domains(&criteria);
        assert_eq!(
            result
                .get(&VerificationDomain::ApiEndpoint)
                .map(|v| v.len()),
            Some(3),
            "All three criteria should be classified as ApiEndpoint"
        );
    }

    #[test]
    fn test_classify_ui_criteria() {
        let criteria = vec![
            make_criterion("u1", "button is rendered", "check button", "general"),
            make_criterion(
                "u2",
                "element is visible on page",
                "look for visible element",
                "general",
            ),
            make_criterion(
                "u3",
                "element appears in DOM",
                "check element presence",
                "general",
            ),
        ];
        let result = classify_criteria_domains(&criteria);
        assert_eq!(
            result.get(&VerificationDomain::UiContent).map(|v| v.len()),
            Some(3),
            "All three criteria should be classified as UiContent"
        );
    }

    #[test]
    fn test_classify_database_criteria() {
        let criteria = vec![
            make_criterion("d1", "table users exists", "check table", "general"),
            make_criterion("d2", "migration runs cleanly", "run migration", "general"),
            make_criterion("d3", "SQL query returns rows", "execute SQL", "general"),
        ];
        let result = classify_criteria_domains(&criteria);
        assert_eq!(
            result
                .get(&VerificationDomain::DatabaseState)
                .map(|v| v.len()),
            Some(3),
            "All three criteria should be classified as DatabaseState"
        );
    }

    #[test]
    fn test_classify_security_criteria() {
        let criteria = vec![
            make_criterion("s1", "auth token is validated", "check auth", "general"),
            make_criterion("s2", "token refresh works", "verify token flow", "general"),
            make_criterion("s3", "CORS headers are set", "check cors", "general"),
        ];
        let result = classify_criteria_domains(&criteria);
        assert_eq!(
            result.get(&VerificationDomain::Security).map(|v| v.len()),
            Some(3),
            "All three criteria should be classified as Security"
        );
    }

    #[test]
    fn test_classify_general_fallback() {
        let criteria = vec![make_criterion(
            "g1",
            "something happens",
            "verify it",
            "general",
        )];
        let result = classify_criteria_domains(&criteria);
        assert_eq!(
            result.get(&VerificationDomain::General).map(|v| v.len()),
            Some(1),
            "Criterion with no matching keywords should fall back to General"
        );
    }

    #[test]
    fn test_category_to_domain_mapping() {
        // Compilation / build
        assert_eq!(
            category_to_domain("compilation"),
            Some(VerificationDomain::Compilation)
        );
        assert_eq!(
            category_to_domain("build"),
            Some(VerificationDomain::Compilation)
        );
        // ApiEndpoint
        assert_eq!(
            category_to_domain("api"),
            Some(VerificationDomain::ApiEndpoint)
        );
        assert_eq!(
            category_to_domain("endpoint"),
            Some(VerificationDomain::ApiEndpoint)
        );
        assert_eq!(
            category_to_domain("api-endpoint"),
            Some(VerificationDomain::ApiEndpoint)
        );
        // UiContent
        assert_eq!(
            category_to_domain("ui"),
            Some(VerificationDomain::UiContent)
        );
        assert_eq!(
            category_to_domain("ui-content"),
            Some(VerificationDomain::UiContent)
        );
        assert_eq!(
            category_to_domain("frontend"),
            Some(VerificationDomain::UiContent)
        );
        // DatabaseState
        assert_eq!(
            category_to_domain("database"),
            Some(VerificationDomain::DatabaseState)
        );
        assert_eq!(
            category_to_domain("db"),
            Some(VerificationDomain::DatabaseState)
        );
        assert_eq!(
            category_to_domain("data"),
            Some(VerificationDomain::DatabaseState)
        );
        // Security
        assert_eq!(
            category_to_domain("security"),
            Some(VerificationDomain::Security)
        );
        assert_eq!(
            category_to_domain("auth"),
            Some(VerificationDomain::Security)
        );
        assert_eq!(
            category_to_domain("authentication"),
            Some(VerificationDomain::Security)
        );
        // Performance
        assert_eq!(
            category_to_domain("performance"),
            Some(VerificationDomain::Performance)
        );
        assert_eq!(
            category_to_domain("perf"),
            Some(VerificationDomain::Performance)
        );
        // Integration
        assert_eq!(
            category_to_domain("integration"),
            Some(VerificationDomain::Integration)
        );
        assert_eq!(
            category_to_domain("e2e"),
            Some(VerificationDomain::Integration)
        );
        // CiCd
        assert_eq!(category_to_domain("ci"), Some(VerificationDomain::CiCd));
        assert_eq!(category_to_domain("cd"), Some(VerificationDomain::CiCd));
        assert_eq!(category_to_domain("cicd"), Some(VerificationDomain::CiCd));
        assert_eq!(category_to_domain("ci-cd"), Some(VerificationDomain::CiCd));
        assert_eq!(
            category_to_domain("deployment"),
            Some(VerificationDomain::CiCd)
        );
        // Unknown
        assert_eq!(category_to_domain("unknown"), None);
        assert_eq!(category_to_domain("random"), None);
    }

    #[test]
    fn test_dependency_order_covers_all_specific() {
        let specific: HashSet<VerificationDomain> =
            VerificationDomain::all_specific().iter().copied().collect();
        let ordered: HashSet<VerificationDomain> = VerificationDomain::dependency_order()
            .iter()
            .copied()
            .collect();
        assert_eq!(
            specific, ordered,
            "dependency_order() must contain exactly the same domains as all_specific()"
        );
    }

    #[test]
    fn test_dependencies_no_cycles() {
        for domain in VerificationDomain::all_specific() {
            let deps = domain.dependencies();
            assert!(!deps.contains(domain), "{:?} depends on itself", domain);
        }
        // General should also not depend on itself
        assert!(
            !VerificationDomain::General
                .dependencies()
                .contains(&VerificationDomain::General),
            "General depends on itself"
        );
    }

    #[test]
    fn test_classify_multiple_criteria_different_domains() {
        let criteria = vec![
            make_criterion("c1", "build succeeds", "cargo build", "general"),
            make_criterion("a1", "endpoint returns 200", "curl /health", "general"),
            make_criterion("u1", "button is visible", "check element", "general"),
            make_criterion("g1", "something works", "verify it", "general"),
        ];
        let result = classify_criteria_domains(&criteria);

        assert!(
            result
                .get(&VerificationDomain::Compilation)
                .is_some_and(|v| v.contains(&"c1".to_string())),
            "c1 should be in Compilation"
        );
        assert!(
            result
                .get(&VerificationDomain::ApiEndpoint)
                .is_some_and(|v| v.contains(&"a1".to_string())),
            "a1 should be in ApiEndpoint"
        );
        assert!(
            result
                .get(&VerificationDomain::UiContent)
                .is_some_and(|v| v.contains(&"u1".to_string())),
            "u1 should be in UiContent"
        );
        assert!(
            result
                .get(&VerificationDomain::General)
                .is_some_and(|v| v.contains(&"g1".to_string())),
            "g1 should be in General"
        );
    }

    #[test]
    fn test_str_to_domain_all_variants() {
        assert_eq!(
            str_to_domain("compilation"),
            Some(VerificationDomain::Compilation)
        );
        assert_eq!(
            str_to_domain("Compilation"),
            Some(VerificationDomain::Compilation)
        );
        assert_eq!(
            str_to_domain("api_endpoint"),
            Some(VerificationDomain::ApiEndpoint)
        );
        assert_eq!(
            str_to_domain("apiendpoint"),
            Some(VerificationDomain::ApiEndpoint)
        );
        assert_eq!(
            str_to_domain("ApiEndpoint"),
            Some(VerificationDomain::ApiEndpoint)
        );
        assert_eq!(
            str_to_domain("ui_content"),
            Some(VerificationDomain::UiContent)
        );
        assert_eq!(
            str_to_domain("uicontent"),
            Some(VerificationDomain::UiContent)
        );
        assert_eq!(
            str_to_domain("database_state"),
            Some(VerificationDomain::DatabaseState)
        );
        assert_eq!(
            str_to_domain("databasestate"),
            Some(VerificationDomain::DatabaseState)
        );
        assert_eq!(
            str_to_domain("security"),
            Some(VerificationDomain::Security)
        );
        assert_eq!(
            str_to_domain("performance"),
            Some(VerificationDomain::Performance)
        );
        assert_eq!(
            str_to_domain("integration"),
            Some(VerificationDomain::Integration)
        );
        assert_eq!(str_to_domain("ci_cd"), Some(VerificationDomain::CiCd));
        assert_eq!(str_to_domain("cicd"), Some(VerificationDomain::CiCd));
        assert_eq!(str_to_domain("CICD"), Some(VerificationDomain::CiCd));
        assert_eq!(str_to_domain("unknown_domain"), None);
        assert_eq!(str_to_domain("general"), None);
        assert_eq!(str_to_domain(""), None);
    }

    #[test]
    fn test_parse_llm_classification_valid_json() {
        let input = r#"{"c1": "compilation", "c2": "api_endpoint", "c3": "security"}"#;
        let result = parse_llm_classification(input).unwrap();
        assert_eq!(result.len(), 3);
        assert_eq!(result["c1"], VerificationDomain::Compilation);
        assert_eq!(result["c2"], VerificationDomain::ApiEndpoint);
        assert_eq!(result["c3"], VerificationDomain::Security);
    }

    #[test]
    fn test_parse_llm_classification_with_markdown_fences() {
        let input = "```json\n{\"c1\": \"compilation\", \"c2\": \"ui_content\"}\n```";
        let result = parse_llm_classification(input).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result["c1"], VerificationDomain::Compilation);
        assert_eq!(result["c2"], VerificationDomain::UiContent);
    }

    #[test]
    fn test_parse_llm_classification_skips_unknown_domains() {
        let input = r#"{"c1": "compilation", "c2": "unknown_thing"}"#;
        let result = parse_llm_classification(input).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result["c1"], VerificationDomain::Compilation);
    }

    #[test]
    fn test_parse_llm_classification_invalid_json() {
        let result = parse_llm_classification("not json at all");
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_llm_classification_empty_object() {
        let input = "{}";
        let result = parse_llm_classification(input).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_domain_description_covers_all() {
        // Ensure every domain variant has a non-empty description
        for domain in VerificationDomain::all_specific() {
            let desc = domain_description(domain);
            assert!(!desc.is_empty(), "{:?} has empty description", domain);
        }
        assert!(!domain_description(&VerificationDomain::General).is_empty());
    }
}
