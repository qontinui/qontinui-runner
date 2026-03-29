//! Domain-Specialized Generators
//!
//! Each verification domain gets a specialized generator with expert-level
//! prompts, domain-specific anti-patterns, and template injection support.
//! Generators produce verification step JSON (as serde_json::Value arrays)
//! for their domain subset of acceptance criteria.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Instant;
use tracing::{debug, info, warn};

use super::domain_routing::VerificationDomain;
use super::generator::extract_json_from_response;
use super::specification::AcceptanceCriterion;
use super::template_library::StepTemplate;
use crate::ai_provider::AiResponse;
use crate::ai_router::TaskContext;
use crate::doctor::DoctorHandle;

// ============================================================================
// Context & result types
// ============================================================================

/// Context provided to domain generators.
#[derive(Debug, Clone)]
pub struct DomainContext {
    /// Output from the discovery phase (file listings, dependency graphs, etc.)
    pub discovery_context: String,
    /// Resolved context from investigation (code snippets, docs, etc.)
    pub resolved_contexts: String,
    /// The original user description / task prompt
    pub user_description: String,
}

/// Result from a domain generator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainGenerationResult {
    pub domain: VerificationDomain,
    pub steps: Vec<Value>,
    pub success: bool,
    pub error: Option<String>,
    pub duration_ms: u64,
}

// ============================================================================
// Factory
// ============================================================================

/// Get the appropriate domain generator for a domain.
pub fn get_domain_generator(domain: VerificationDomain) -> Box<dyn DomainGenerator> {
    match domain {
        VerificationDomain::Compilation => Box::new(CompilationGenerator),
        VerificationDomain::ApiEndpoint => Box::new(ApiEndpointGenerator),
        VerificationDomain::UiContent => Box::new(UiContentGenerator),
        VerificationDomain::DatabaseState => Box::new(DatabaseStateGenerator),
        VerificationDomain::Security => Box::new(SecurityGenerator),
        VerificationDomain::Performance => Box::new(PerformanceGenerator),
        VerificationDomain::Integration => Box::new(IntegrationGenerator),
        VerificationDomain::CiCd => Box::new(CiCdGenerator),
        VerificationDomain::General => Box::new(GeneralGenerator),
    }
}

// ============================================================================
// Trait
// ============================================================================

/// Trait for domain-specialized step generators.
pub trait DomainGenerator: Send + Sync {
    /// Which domain this generator handles.
    fn domain(&self) -> VerificationDomain;

    /// Domain expert system prompt content — injected at the top of the prompt
    /// so the AI adopts the right mental model.
    fn system_knowledge(&self) -> &str;

    /// Known anti-patterns the AI should avoid for this domain.
    fn anti_patterns(&self) -> &[&str];

    /// Build the full generation prompt for this domain.
    fn build_prompt(
        &self,
        criteria: &[&AcceptanceCriterion],
        context: &DomainContext,
        templates: &[StepTemplate],
    ) -> String {
        let mut prompt = String::with_capacity(8192);

        // ---- 1. System role with domain expertise ----
        prompt.push_str(&format!(
            "You are a verification-step generator specializing in the **{}** domain.\n\n",
            self.domain()
        ));
        prompt.push_str("## Domain Expertise\n\n");
        prompt.push_str(self.system_knowledge());
        prompt.push_str("\n\n");

        // ---- 2. Anti-patterns ----
        let anti = self.anti_patterns();
        if !anti.is_empty() {
            prompt.push_str("## Anti-Patterns — DO NOT generate steps that:\n\n");
            for ap in anti {
                prompt.push_str(&format!("- {}\n", ap));
            }
            prompt.push_str("\n");
        }

        // ---- 3. Template injection ----
        if !templates.is_empty() {
            prompt.push_str("## Available Templates\n\n");
            prompt.push_str(
                "Re-use these proven templates where they match a criterion. \
                 Replace `{{placeholders}}` with concrete values.\n\n",
            );
            for t in templates {
                let cmd_summary = t.template_steps.first()
                    .and_then(|s| s.command_template.as_deref())
                    .unwrap_or("(no command)");
                prompt.push_str(&format!(
                    "- **{}** — {} (confidence: {:.0}%) — `{}`\n",
                    t.id, t.pattern_description, t.confidence * 100.0, cmd_summary
                ));
            }
            prompt.push_str("\n");
        }

        // ---- 4. Discovery & resolved context ----
        if !context.discovery_context.is_empty() {
            prompt.push_str("## Discovery Context\n\n");
            // Truncate to avoid blowing token budget
            let disc = if context.discovery_context.len() > 4000 {
                &context.discovery_context[..4000]
            } else {
                &context.discovery_context
            };
            prompt.push_str(disc);
            prompt.push_str("\n\n");
        }
        if !context.resolved_contexts.is_empty() {
            prompt.push_str("## Resolved Investigation Context\n\n");
            let res = if context.resolved_contexts.len() > 4000 {
                &context.resolved_contexts[..4000]
            } else {
                &context.resolved_contexts
            };
            prompt.push_str(res);
            prompt.push_str("\n\n");
        }

        // ---- 5. User description ----
        prompt.push_str("## Task Description\n\n");
        prompt.push_str(&context.user_description);
        prompt.push_str("\n\n");

        // ---- 6. Criteria list ----
        prompt.push_str("## Acceptance Criteria to Verify\n\n");
        for (i, c) in criteria.iter().enumerate() {
            prompt.push_str(&format!(
                "{}. **{}** (priority: {:?}, method: {:?})\n   {}\n   Hint: {}\n\n",
                i + 1,
                c.id,
                c.priority,
                c.method,
                c.description,
                c.verification_hint
            ));
        }

        // ---- 7. Output format instructions ----
        prompt.push_str("## Output Format\n\n");
        prompt.push_str(
            "Return a JSON array of verification step objects. Each object MUST have:\n\n\
             ```\n\
             {\n\
               \"name\": \"<kebab-case step name>\",\n\
               \"step_type\": \"command\" | \"prompt\",\n\
               \"command\": \"<shell command to run>\",\n\
               \"check_type\": \"exit_code\" | \"stdout_contains\" | \"stderr_not_contains\" | \"json_field\",\n\
               \"expected\": \"<expected value or substring>\",\n\
               \"depends_on\": [\"<name of prerequisite step>\"] | [],\n\
               \"description\": \"<one-line human explanation>\"\n\
             }\n\
             ```\n\n\
             Rules:\n\
             - `step_type` is \"command\" for shell commands, \"prompt\" for AI-evaluated checks.\n\
             - `check_type` + `expected` define the pass condition:\n\
               - \"exit_code\": `expected` is \"0\" for success.\n\
               - \"stdout_contains\": command stdout must contain `expected` substring.\n\
               - \"stderr_not_contains\": command stderr must NOT contain `expected` substring.\n\
               - \"json_field\": parse stdout as JSON, check the field path in `expected` (format: \"field.path=value\").\n\
             - `depends_on` lists step names that must pass before this step runs.\n\
             - Use concrete, runnable commands — no pseudocode, no placeholders.\n\
             - Prefer deterministic checks (exit codes, exact strings) over fuzzy checks.\n\
             - Return ONLY the JSON array, no markdown wrapping, no commentary.\n",
        );

        prompt
    }

    /// Generate verification steps for this domain's criteria.
    fn generate(
        &self,
        criteria: &[&AcceptanceCriterion],
        context: &DomainContext,
        templates: &[StepTemplate],
        doctor_handle: Option<&DoctorHandle>,
        model_override: Option<&str>,
        provider_override: Option<&str>,
    ) -> DomainGenerationResult {
        let domain = self.domain();
        let start = Instant::now();

        if criteria.is_empty() {
            debug!("No criteria for domain {}, skipping generation", domain);
            return DomainGenerationResult {
                domain,
                steps: vec![],
                success: true,
                error: None,
                duration_ms: 0,
            };
        }

        info!(
            "Generating {} verification steps for {} criteria",
            domain,
            criteria.len()
        );

        let prompt = self.build_prompt(criteria, context, templates);
        let task_context = TaskContext::from_prompt(&prompt)
            .with_criteria_count(criteria.len());

        let ai_result: AiResponse = crate::ai_provider::run_prompt_with_model_override(
            &prompt,
            &task_context,
            doctor_handle,
            model_override,
            provider_override,
            None, // temperature
            None, // max_tokens
            None, // fallback_model
            None, // fallback_provider
        );

        let duration_ms = start.elapsed().as_millis() as u64;

        if !ai_result.success {
            let err = ai_result.error.unwrap_or_else(|| "unknown error".into());
            warn!("Domain {} generation failed: {}", domain, err);
            return DomainGenerationResult {
                domain,
                steps: vec![],
                success: false,
                error: Some(err),
                duration_ms,
            };
        }

        // Parse JSON array from response
        let json_text = extract_json_from_response(&ai_result.output);
        match serde_json::from_str::<Vec<Value>>(&json_text) {
            Ok(steps) => {
                info!(
                    "Domain {} produced {} steps in {}ms",
                    domain,
                    steps.len(),
                    duration_ms
                );
                DomainGenerationResult {
                    domain,
                    steps,
                    success: true,
                    error: None,
                    duration_ms,
                }
            }
            Err(e) => {
                // Try wrapping in array if it parsed as a single object
                if let Ok(single) = serde_json::from_str::<Value>(&json_text) {
                    if single.is_object() {
                        info!(
                            "Domain {} returned single object, wrapping in array ({}ms)",
                            domain, duration_ms
                        );
                        return DomainGenerationResult {
                            domain,
                            steps: vec![single],
                            success: true,
                            error: None,
                            duration_ms,
                        };
                    }
                }
                warn!(
                    "Domain {} JSON parse failed: {} — raw: {}",
                    domain,
                    e,
                    &json_text[..json_text.len().min(200)]
                );
                DomainGenerationResult {
                    domain,
                    steps: vec![],
                    success: false,
                    error: Some(format!("JSON parse error: {}", e)),
                    duration_ms,
                }
            }
        }
    }
}

// ============================================================================
// Concrete generators
// ============================================================================

struct CompilationGenerator;

impl DomainGenerator for CompilationGenerator {
    fn domain(&self) -> VerificationDomain {
        VerificationDomain::Compilation
    }

    fn system_knowledge(&self) -> &str {
        "You are an expert in build systems and compilation toolchains.\n\
         Key principles:\n\
         - Compilation checks should run first since all other domains depend on buildable code.\n\
         - Always check with the project's native build tool (cargo, npm, go build, etc.).\n\
         - Distinguish between type errors, lint warnings, and formatting issues — they need separate steps.\n\
         - Use `--no-emit` / `--check` / `--noEmit` flags when you only need to verify compilation, not produce artifacts.\n\
         - For monorepos, scope compilation to the affected workspace/package rather than building everything.\n\
         - Capture stderr separately — compilers often output warnings to stderr even on success.\n\
         - Set a reasonable timeout; compilation of large projects can take minutes.\n\
         - Check for lock file consistency (Cargo.lock, package-lock.json) when dependencies are involved."
    }

    fn anti_patterns(&self) -> &[&str] {
        &[
            "Run a full production build when only a type-check is needed",
            "Ignore compiler warnings — they should be captured even if non-fatal",
            "Assume the working directory is the project root without verifying",
            "Use `npm install` as a build step — it is dependency resolution, not compilation",
            "Skip checking for circular dependencies which can cause silent build failures",
        ]
    }
}

struct ApiEndpointGenerator;

impl DomainGenerator for ApiEndpointGenerator {
    fn domain(&self) -> VerificationDomain {
        VerificationDomain::ApiEndpoint
    }

    fn system_knowledge(&self) -> &str {
        "You are an expert in REST/GraphQL API verification.\n\
         Key principles:\n\
         - Use `curl -s -o /dev/null -w '%{http_code}'` to check status codes without noisy output.\n\
         - Always verify both the status code AND the response body structure.\n\
         - For JSON responses, use `curl ... | python -c \"import sys,json; ...\"` to validate fields.\n\
         - Test error paths too: invalid input should return 4xx, not 5xx or 200.\n\
         - Use `-H 'Content-Type: application/json'` for POST/PUT/PATCH requests.\n\
         - Include authentication headers when the endpoint requires them.\n\
         - Check response headers (CORS, Content-Type, Cache-Control) when relevant.\n\
         - Retry with a short backoff for endpoints that depend on service startup.\n\
         - Never hardcode tokens in commands; reference environment variables or config files."
    }

    fn anti_patterns(&self) -> &[&str] {
        &[
            "Use `wget` instead of `curl` — curl is more portable and scriptable",
            "Ignore response body and only check status code for endpoints that return data",
            "Hardcode authentication tokens directly in the command string",
            "Skip testing error responses (4xx/5xx) and only verify happy paths",
            "Use `grep` to parse JSON instead of a proper JSON parser (python -c, jq)",
        ]
    }
}

struct UiContentGenerator;

impl DomainGenerator for UiContentGenerator {
    fn domain(&self) -> VerificationDomain {
        VerificationDomain::UiContent
    }

    fn system_knowledge(&self) -> &str {
        "You are an expert in UI verification and frontend testing.\n\
         Key principles:\n\
         - Prefer UI Bridge SDK queries over raw DOM scraping for Tauri/Electron apps.\n\
         - Check that elements are both present AND visible — an element can exist in DOM but be hidden.\n\
         - Verify text content with substring matching rather than exact equality to handle whitespace.\n\
         - For interactive elements (buttons, links), verify they have accessible labels.\n\
         - Test responsive breakpoints separately if layout requirements exist.\n\
         - Wait for dynamic content to load before asserting — use polling or retry patterns.\n\
         - Check for loading/error states, not just the happy-path rendered state.\n\
         - Visual regression tests need a baseline; if none exists, the step should create one.\n\
         - Verify that user interactions (click, type) produce the expected state changes."
    }

    fn anti_patterns(&self) -> &[&str] {
        &[
            "Assert exact pixel positions — layout shifts between environments",
            "Check only element existence without verifying visible text content",
            "Use fragile CSS selectors (nth-child, deeply nested paths) instead of data-testid or role",
            "Skip waiting for async content and assert immediately after navigation",
            "Assume a specific viewport size without setting it explicitly",
        ]
    }
}

struct DatabaseStateGenerator;

impl DomainGenerator for DatabaseStateGenerator {
    fn domain(&self) -> VerificationDomain {
        VerificationDomain::DatabaseState
    }

    fn system_knowledge(&self) -> &str {
        "You are an expert in database verification and schema integrity.\n\
         Key principles:\n\
         - Always verify migrations apply cleanly on a fresh database before checking schema state.\n\
         - Use the database CLI tool (psql, sqlite3, mysql) for direct queries.\n\
         - Check table existence, column types, constraints, and indexes separately.\n\
         - Verify foreign key relationships and cascade behavior, not just column presence.\n\
         - For data integrity, use COUNT/SUM aggregates rather than dumping full tables.\n\
         - Test rollback behavior — if a migration has a `down`, verify it reverses cleanly.\n\
         - Use transactions for test data insertion so it can be rolled back without side effects.\n\
         - Check that NOT NULL constraints, unique indexes, and check constraints are enforced."
    }

    fn anti_patterns(&self) -> &[&str] {
        &[
            "SELECT * from large tables — use targeted queries with WHERE and LIMIT",
            "Skip testing migration rollback (down) when the migration defines one",
            "Assume the database is empty — always scope queries to test-specific data",
            "Use string comparison for dates/timestamps instead of proper date functions",
            "Forget to close database connections or transactions in verification commands",
        ]
    }
}

struct SecurityGenerator;

impl DomainGenerator for SecurityGenerator {
    fn domain(&self) -> VerificationDomain {
        VerificationDomain::Security
    }

    fn system_knowledge(&self) -> &str {
        "You are an expert in application security verification.\n\
         Key principles:\n\
         - Test authentication by attempting access both with and without valid credentials.\n\
         - Verify authorization by testing that users cannot access resources above their role.\n\
         - Check CORS headers: Origin, Methods, Headers, Credentials settings.\n\
         - Test input validation by sending known-bad payloads (SQL injection, XSS vectors).\n\
         - Verify that sensitive data (passwords, tokens) is never returned in API responses.\n\
         - Check that security headers are set (X-Frame-Options, CSP, HSTS, X-Content-Type-Options).\n\
         - Verify rate limiting by sending rapid sequential requests.\n\
         - Test session management: expiration, invalidation on logout, secure cookie flags.\n\
         - Never use real credentials in tests; use dedicated test accounts or mock tokens."
    }

    fn anti_patterns(&self) -> &[&str] {
        &[
            "Only test the happy path (valid credentials) without testing rejection of invalid ones",
            "Use real production secrets or API keys in verification commands",
            "Skip testing authorization (role-based access) and only test authentication (login)",
            "Assume HTTPS by checking only the application layer without verifying TLS configuration",
            "Test only one injection vector — verify SQL, XSS, and command injection separately",
        ]
    }
}

struct PerformanceGenerator;

impl DomainGenerator for PerformanceGenerator {
    fn domain(&self) -> VerificationDomain {
        VerificationDomain::Performance
    }

    fn system_knowledge(&self) -> &str {
        "You are an expert in performance verification and benchmarking.\n\
         Key principles:\n\
         - Use `curl -w '%{time_total}'` or `time` to measure response latency.\n\
         - Always define explicit thresholds (e.g., <200ms p95) rather than subjective 'fast'.\n\
         - Run multiple iterations (3-5 minimum) and check the worst case, not the average.\n\
         - Warm up the service before measuring — first requests include cold-start overhead.\n\
         - Memory checks should use `ps`, `/proc`, or platform tools, not application self-reporting.\n\
         - For load testing, use simple `seq` + `curl` loops or `ab` rather than heavy frameworks.\n\
         - Separate network latency from processing time when possible.\n\
         - Set generous but finite timeouts to catch hangs without false positives."
    }

    fn anti_patterns(&self) -> &[&str] {
        &[
            "Measure a single request and treat it as representative — always use multiple samples",
            "Use wall-clock time that includes unrelated system overhead instead of request-level timing",
            "Set unrealistically tight thresholds that fail in CI due to shared-runner variability",
            "Run performance tests concurrently with compilation or other heavy tasks",
        ]
    }
}

struct IntegrationGenerator;

impl DomainGenerator for IntegrationGenerator {
    fn domain(&self) -> VerificationDomain {
        VerificationDomain::Integration
    }

    fn system_knowledge(&self) -> &str {
        "You are an expert in integration testing between services and components.\n\
         Key principles:\n\
         - Verify that upstream services are reachable before testing the integration itself.\n\
         - Use health-check endpoints (GET /health, /readyz) as preconditions.\n\
         - Test the full request-response cycle: send a message, verify it was received and processed.\n\
         - For async integrations (queues, events), poll for the expected state with a timeout.\n\
         - Check error propagation: when an upstream fails, does the service degrade gracefully?\n\
         - Verify retry and circuit-breaker behavior if the system claims to support them.\n\
         - Use docker-compose or similar to ensure dependent services are running.\n\
         - Check that service discovery / configuration resolves correctly in the test environment."
    }

    fn anti_patterns(&self) -> &[&str] {
        &[
            "Test integration logic by mocking the external service — that is a unit test, not integration",
            "Skip health-check preconditions and let the integration step fail with a confusing network error",
            "Assume services start instantly — always add readiness polling before integration checks",
            "Ignore timeout configuration — integration tests need longer timeouts than unit tests",
        ]
    }
}

struct CiCdGenerator;

impl DomainGenerator for CiCdGenerator {
    fn domain(&self) -> VerificationDomain {
        VerificationDomain::CiCd
    }

    fn system_knowledge(&self) -> &str {
        "You are an expert in CI/CD pipeline verification.\n\
         Key principles:\n\
         - Verify pipeline configuration files (YAML/TOML) parse correctly before testing execution.\n\
         - Check that required secrets and environment variables are defined (exist, not their values).\n\
         - Verify Dockerfile builds successfully with `docker build --check` or a dry-run.\n\
         - Test that build artifacts are produced in the expected location and format.\n\
         - Check that deployment manifests (k8s, docker-compose) are valid with lint tools.\n\
         - Verify that the pipeline triggers on the correct events (push, PR, tag).\n\
         - For GitHub Actions, use `act` for local validation or check workflow syntax.\n\
         - Ensure artifact retention and cleanup policies are configured."
    }

    fn anti_patterns(&self) -> &[&str] {
        &[
            "Actually deploy to production as a verification step — use dry-run or staging",
            "Check secrets by printing their values — only verify they are set (non-empty)",
            "Skip validating YAML syntax and jump straight to pipeline execution",
            "Assume Docker is available without checking — CI runners may use rootless or podman",
        ]
    }
}

struct GeneralGenerator;

impl DomainGenerator for GeneralGenerator {
    fn domain(&self) -> VerificationDomain {
        VerificationDomain::General
    }

    fn system_knowledge(&self) -> &str {
        "You are a generalist verification step generator.\n\
         Key principles:\n\
         - When a criterion does not fit a specific domain, prefer simple shell commands.\n\
         - File existence checks use `test -f` or `test -d`, not `ls`.\n\
         - Content checks use `python -c` for cross-platform string/JSON operations.\n\
         - Prefer `grep -q` for presence checks in text files (returns exit code, no output).\n\
         - For configuration verification, parse the config file format (JSON, YAML, TOML) properly.\n\
         - When in doubt, check exit codes — they are the most reliable pass/fail signal.\n\
         - Keep commands simple and composable — one check per step.\n\
         - Use standard POSIX utilities where possible for portability."
    }

    fn anti_patterns(&self) -> &[&str] {
        &[
            "Combine multiple unrelated checks into one mega-command with && chains",
            "Use platform-specific commands (e.g., `reg query` on Windows) without a portability wrapper",
            "Write verification commands that modify state — verification should be read-only",
            "Rely on specific file line numbers which change as code evolves",
        ]
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::specification::{CriterionPriority, VerificationMethod};

    #[test]
    fn test_get_domain_generator_returns_correct_domain() {
        for domain in VerificationDomain::dependency_order() {
            let gen = get_domain_generator(*domain);
            assert_eq!(gen.domain(), *domain);
        }
        let gen = get_domain_generator(VerificationDomain::General);
        assert_eq!(gen.domain(), VerificationDomain::General);
    }

    #[test]
    fn test_build_prompt_contains_criteria() {
        let gen = get_domain_generator(VerificationDomain::Compilation);
        let criterion = AcceptanceCriterion {
            id: "typecheck-passes".to_string(),
            description: "TypeScript compilation succeeds".to_string(),
            method: VerificationMethod::Command,
            priority: CriterionPriority::Critical,
            verification_hint: "Run npx tsc --noEmit".to_string(),
            category: "compilation".to_string(),
        };
        let context = DomainContext {
            discovery_context: "Found package.json with typescript dep".to_string(),
            resolved_contexts: "tsconfig.json uses strict mode".to_string(),
            user_description: "Add new TypeScript module".to_string(),
        };
        let prompt = gen.build_prompt(&[&criterion], &context, &[]);
        assert!(prompt.contains("typecheck-passes"));
        assert!(prompt.contains("npx tsc --noEmit"));
        assert!(prompt.contains("Compilation"));
        assert!(prompt.contains("Anti-Patterns"));
        assert!(prompt.contains("JSON array"));
    }

    #[test]
    fn test_build_prompt_includes_templates() {
        let gen = get_domain_generator(VerificationDomain::ApiEndpoint);
        let criterion = AcceptanceCriterion {
            id: "health-endpoint".to_string(),
            description: "Health endpoint returns 200".to_string(),
            method: VerificationMethod::Command,
            priority: CriterionPriority::Critical,
            verification_hint: "curl /health".to_string(),
            category: "api".to_string(),
        };
        let context = DomainContext {
            discovery_context: String::new(),
            resolved_contexts: String::new(),
            user_description: "Add health endpoint".to_string(),
        };
        let template = StepTemplate {
            id: "curl-status-check".to_string(),
            domain: VerificationDomain::ApiEndpoint,
            pattern_description: "Check HTTP status code".to_string(),
            template_steps: vec![crate::workflow_generation::template_library::TemplateStep {
                step_type: "command".to_string(),
                command_template: Some("curl -s -o /dev/null -w '%{http_code}' {{url}}".to_string()),
                check_type: Some("exit_code".to_string()),
                expected_template: Some("0".to_string()),
                description: "Check HTTP status code".to_string(),
            }],
            parameters: vec![],
            success_count: 5,
            failure_count: 0,
            confidence: 0.9,
            source: crate::workflow_generation::template_library::TemplateSource::Seeded,
        };
        let prompt = gen.build_prompt(&[&criterion], &context, &[template]);
        assert!(prompt.contains("curl-status-check"));
        assert!(prompt.contains("Available Templates"));
    }

    #[test]
    fn test_generate_empty_criteria_returns_success() {
        let gen = get_domain_generator(VerificationDomain::Security);
        let context = DomainContext {
            discovery_context: String::new(),
            resolved_contexts: String::new(),
            user_description: "test".to_string(),
        };
        let result = gen.generate(&[], &context, &[], None, None, None);
        assert!(result.success);
        assert!(result.steps.is_empty());
        assert_eq!(result.duration_ms, 0);
    }

    #[test]
    fn test_all_generators_have_nonempty_knowledge_and_antipatterns() {
        let domains = [
            VerificationDomain::Compilation,
            VerificationDomain::ApiEndpoint,
            VerificationDomain::UiContent,
            VerificationDomain::DatabaseState,
            VerificationDomain::Security,
            VerificationDomain::Performance,
            VerificationDomain::Integration,
            VerificationDomain::CiCd,
            VerificationDomain::General,
        ];
        for d in &domains {
            let gen = get_domain_generator(*d);
            assert!(
                !gen.system_knowledge().is_empty(),
                "{:?} has empty system_knowledge",
                d
            );
            assert!(
                !gen.anti_patterns().is_empty(),
                "{:?} has empty anti_patterns",
                d
            );
        }
    }
}
