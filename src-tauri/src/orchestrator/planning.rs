//! Planning Agent for the task orchestrator.
//!
//! The Planning Agent is responsible for:
//! 1. Analyzing the user's goal/task description
//! 2. Decomposing it into measurable success criteria
//! 3. Creating a verification plan with deterministic and AI-evaluated criteria
//! 4. Suggesting worker configuration (single vs multi-worker)

use serde::{Deserialize, Serialize};
use tracing::info;

use crate::ai_router::TaskContext;
use super::types::{
    CriterionType, SuccessCriterion, VerificationMethod, VerificationPlan, WorkerDomain,
};

/// Context provided to the planning agent for goal analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanningContext {
    /// The user's goal/task description
    pub goal: String,

    /// Available verification methods in this environment
    pub available_methods: Vec<VerificationMethodInfo>,

    /// Project type hints (detected from workspace)
    pub project_hints: ProjectHints,

    /// Execution steps already configured (if any)
    pub configured_execution_steps: Option<serde_json::Value>,

    /// Previous plan version (for replanning)
    pub previous_plan: Option<VerificationPlan>,

    /// Reason for replanning (if applicable)
    pub replan_reason: Option<String>,
}

/// Information about an available verification method.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationMethodInfo {
    pub method: VerificationMethod,
    pub description: String,
    pub available: bool,
    pub config_hints: Option<String>,
}

/// Hints about the project type for planning.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProjectHints {
    /// Has package.json (npm/node project)
    pub has_npm: bool,
    /// Has Cargo.toml (Rust project)
    pub has_cargo: bool,
    /// Has pyproject.toml or setup.py (Python project)
    pub has_python: bool,
    /// Detected test frameworks
    pub test_frameworks: Vec<String>,
    /// Has TypeScript
    pub has_typescript: bool,
    /// Has Playwright configured
    pub has_playwright: bool,
}

impl ProjectHints {
    /// Detect project hints from a workspace path.
    pub fn detect(workspace_root: &str) -> Self {
        let workspace = std::path::Path::new(workspace_root);

        Self {
            has_npm: workspace.join("package.json").exists(),
            has_cargo: workspace.join("Cargo.toml").exists(),
            has_python: workspace.join("pyproject.toml").exists()
                || workspace.join("setup.py").exists(),
            has_typescript: workspace.join("tsconfig.json").exists(),
            has_playwright: workspace.join("playwright.config.ts").exists()
                || workspace.join("playwright.config.js").exists(),
            test_frameworks: Self::detect_test_frameworks(workspace),
        }
    }

    fn detect_test_frameworks(workspace: &std::path::Path) -> Vec<String> {
        let mut frameworks = Vec::new();

        // Check package.json for test dependencies
        if let Ok(content) = std::fs::read_to_string(workspace.join("package.json")) {
            if content.contains("\"jest\"") || content.contains("\"@jest/") {
                frameworks.push("jest".to_string());
            }
            if content.contains("\"vitest\"") {
                frameworks.push("vitest".to_string());
            }
            if content.contains("\"mocha\"") {
                frameworks.push("mocha".to_string());
            }
            if content.contains("\"playwright\"") || content.contains("\"@playwright/") {
                frameworks.push("playwright".to_string());
            }
        }

        // Check for Rust tests
        if workspace.join("Cargo.toml").exists() {
            frameworks.push("cargo-test".to_string());
        }

        // Check for Python tests
        if workspace.join("pytest.ini").exists() || workspace.join("pyproject.toml").exists() {
            frameworks.push("pytest".to_string());
        }

        frameworks
    }
}

/// Build the system prompt for the planning agent.
pub fn build_planning_system_prompt() -> String {
    r#"You are a Planning Agent for an automated development system. Your job is to analyze goals and create verification plans.

## Your Role

You receive a user's goal and must decompose it into:
1. **Success Criteria** - Specific, measurable conditions that prove the goal is achieved
2. **Verification Methods** - How each criterion will be checked (deterministic or AI-evaluated)
3. **Execution Steps** - Any GUI automation needed before verification (screenshots, clicks, etc.)

## Output Format

You MUST respond with valid JSON in this exact structure:

```json
{
  "goal_summary": "Brief one-sentence summary of the goal",
  "success_criteria": [
    {
      "id": "unique_id",
      "description": "What must be true for this criterion",
      "type": "deterministic" | "ai_evaluated",
      "verification_method": "build_success" | "unit_test" | "integration_test" | "playwright" | "log_pattern" | "type_check" | "lint_check" | "custom_command",
      "verification_config": { ... },
      "evaluation_prompt": "For AI criteria: what to look for in screenshot",
      "required": true | false,
      "is_critical": true | false
    }
  ],
  "execution_steps": [
    {
      "type": "screenshot" | "workflow" | "playwright",
      "name": "Step description",
      "config": { ... }
    }
  ],
  "suggested_worker_count": 1,
  "worker_domains": null
}
```

## Criterion Types

### Deterministic (Preferred)
Use deterministic criteria whenever possible - they're faster and more reliable:
- `build_success` - Code compiles/builds without errors
- `unit_test` - Specific unit tests pass
- `integration_test` - Integration tests pass
- `type_check` - No type errors (TypeScript, mypy, etc.)
- `lint_check` - No lint errors
- `log_pattern` - Specific pattern appears/doesn't appear in logs
- `playwright` - Playwright test script passes
- `custom_command` - Custom shell command exits successfully

### AI-Evaluated (When Necessary)
Use AI evaluation only when deterministic checks aren't sufficient:
- Visual appearance verification (colors, layout, spacing)
- Subjective quality assessment
- Complex state that's hard to test programmatically

## is_critical Flag

Each criterion has an `is_critical` field:
- `true` (default) - Failure BLOCKS task completion. Worker must fix.
- `false` - Failure is INFORMATIONAL. Reported but doesn't block.

Use `is_critical: false` for:
- Style/formatting checks
- Best-practice warnings
- Nice-to-have improvements

## Guidelines

1. **Be Specific**: "Login form shows error message" is better than "Form works correctly"
2. **Prefer Deterministic**: If something can be tested with code, use deterministic verification
3. **Include Build Check**: Almost always include a build_success criterion
4. **Test What Changed**: Focus criteria on the areas the goal affects
5. **Provide Config**: Include necessary config (test patterns, commands, etc.)
6. **Single Worker Default**: Unless the task clearly spans multiple domains, use 1 worker

## Example

Goal: "Fix the login form to show inline validation errors"

```json
{
  "goal_summary": "Add inline validation error display to login form",
  "success_criteria": [
    {
      "id": "build_passes",
      "description": "Application builds without errors",
      "type": "deterministic",
      "verification_method": "build_success",
      "required": true,
      "is_critical": true
    },
    {
      "id": "type_check_passes",
      "description": "No TypeScript type errors",
      "type": "deterministic",
      "verification_method": "type_check",
      "verification_config": { "command": "npm run typecheck" },
      "required": true,
      "is_critical": true
    },
    {
      "id": "validation_tests_pass",
      "description": "Login validation unit tests pass",
      "type": "deterministic",
      "verification_method": "unit_test",
      "verification_config": { "test_pattern": "**/login*.test.ts" },
      "required": true,
      "is_critical": true
    },
    {
      "id": "visual_error_display",
      "description": "Error messages appear correctly below invalid fields",
      "type": "ai_evaluated",
      "evaluation_prompt": "Verify the login form screenshot shows: 1) Red border on email field 2) Error message 'Invalid email' visible below field 3) Submit button disabled or grayed out",
      "required": true,
      "is_critical": true
    },
    {
      "id": "lint_clean",
      "description": "No new lint warnings introduced",
      "type": "deterministic",
      "verification_method": "lint_check",
      "verification_config": { "command": "npm run lint" },
      "required": false,
      "is_critical": false
    }
  ],
  "execution_steps": [
    {
      "type": "playwright",
      "name": "Navigate to login and enter invalid email",
      "config": {
        "script": "Navigate to /login, enter 'invalid-email' in email field, click outside"
      }
    },
    {
      "type": "screenshot",
      "name": "Capture login form state",
      "config": { "delay": 500 }
    }
  ],
  "suggested_worker_count": 1,
  "worker_domains": null
}
```

RESPOND ONLY WITH JSON. No explanation text before or after."#.to_string()
}

/// Build the user prompt for planning with context.
pub fn build_planning_user_prompt(context: &PlanningContext) -> String {
    let mut prompt = String::new();

    // Add goal
    prompt.push_str(&format!("## Goal\n\n{}\n\n", context.goal));

    // Add project hints
    prompt.push_str("## Project Environment\n\n");
    if context.project_hints.has_npm {
        prompt.push_str("- Node.js project (package.json detected)\n");
    }
    if context.project_hints.has_cargo {
        prompt.push_str("- Rust project (Cargo.toml detected)\n");
    }
    if context.project_hints.has_python {
        prompt.push_str("- Python project detected\n");
    }
    if context.project_hints.has_typescript {
        prompt.push_str("- TypeScript enabled\n");
    }
    if context.project_hints.has_playwright {
        prompt.push_str("- Playwright configured\n");
    }
    if !context.project_hints.test_frameworks.is_empty() {
        prompt.push_str(&format!(
            "- Test frameworks: {}\n",
            context.project_hints.test_frameworks.join(", ")
        ));
    }
    prompt.push('\n');

    // Add available verification methods
    prompt.push_str("## Available Verification Methods\n\n");
    for method in &context.available_methods {
        if method.available {
            prompt.push_str(&format!("- **{:?}**: {}\n", method.method, method.description));
            if let Some(hints) = &method.config_hints {
                prompt.push_str(&format!("  Config: {}\n", hints));
            }
        }
    }
    prompt.push('\n');

    // Add existing execution steps if any
    if let Some(steps) = &context.configured_execution_steps {
        prompt.push_str("## Pre-configured Execution Steps\n\n");
        prompt.push_str(&format!("```json\n{}\n```\n\n", serde_json::to_string_pretty(steps).unwrap_or_default()));
        prompt.push_str("These steps are already configured. You can reference them or add more.\n\n");
    }

    // Add replanning context if applicable
    if let Some(previous) = &context.previous_plan {
        prompt.push_str("## Previous Plan (Replanning Requested)\n\n");
        prompt.push_str(&format!("Previous goal summary: {}\n\n", previous.goal_summary));
        if let Some(reason) = &context.replan_reason {
            prompt.push_str(&format!("**Replan reason**: {}\n\n", reason));
        }
        prompt.push_str("Create a revised plan that addresses the replan reason.\n\n");
    }

    prompt.push_str("Create a verification plan for this goal. Respond with JSON only.");

    prompt
}

/// Parse the planning agent's response into a VerificationPlan.
pub fn parse_planning_response(response: &str) -> Result<VerificationPlan, String> {
    // Try to extract JSON from the response
    let json_str = if let Some(start) = response.find('{') {
        if let Some(end) = response.rfind('}') {
            &response[start..=end]
        } else {
            return Err("No closing brace found in response".to_string());
        }
    } else {
        return Err("No JSON found in response".to_string());
    };

    // Parse the JSON
    let parsed: serde_json::Value =
        serde_json::from_str(json_str).map_err(|e| format!("Failed to parse JSON: {}", e))?;

    // Extract fields
    let goal_summary = parsed["goal_summary"]
        .as_str()
        .ok_or("Missing goal_summary")?
        .to_string();

    let success_criteria = parse_success_criteria(&parsed["success_criteria"])?;

    let execution_steps = parsed["execution_steps"]
        .as_array()
        .map(|arr| arr.iter().cloned().collect())
        .unwrap_or_default();

    let suggested_worker_count = parsed["suggested_worker_count"]
        .as_u64()
        .unwrap_or(1) as u32;

    let worker_domains = parse_worker_domains(&parsed["worker_domains"]);

    Ok(VerificationPlan {
        goal_summary,
        success_criteria,
        execution_steps,
        suggested_worker_count,
        worker_domains,
        version: 1,
    })
}

fn parse_success_criteria(value: &serde_json::Value) -> Result<Vec<SuccessCriterion>, String> {
    let arr = value.as_array().ok_or("success_criteria must be an array")?;
    let mut criteria = Vec::new();

    for item in arr {
        let criterion = SuccessCriterion {
            id: item["id"]
                .as_str()
                .ok_or("Criterion missing id")?
                .to_string(),
            description: item["description"]
                .as_str()
                .ok_or("Criterion missing description")?
                .to_string(),
            criterion_type: match item["type"].as_str() {
                Some("deterministic") => CriterionType::Deterministic,
                Some("ai_evaluated") => CriterionType::AiEvaluated,
                _ => CriterionType::Deterministic, // Default to deterministic
            },
            verification_method: parse_verification_method(item["verification_method"].as_str()),
            verification_config: item.get("verification_config").cloned(),
            evaluation_prompt: item["evaluation_prompt"].as_str().map(|s| s.to_string()),
            required: item["required"].as_bool().unwrap_or(true),
            is_critical: item["is_critical"].as_bool().unwrap_or(true), // Default to critical
            weight: item["weight"].as_f64(),
            domain: item["domain"].as_str().map(|s| s.to_string()),
        };
        criteria.push(criterion);
    }

    Ok(criteria)
}

fn parse_verification_method(method: Option<&str>) -> Option<VerificationMethod> {
    match method {
        Some("build_success") => Some(VerificationMethod::BuildSuccess),
        Some("unit_test") => Some(VerificationMethod::UnitTest),
        Some("integration_test") => Some(VerificationMethod::IntegrationTest),
        Some("playwright") => Some(VerificationMethod::Playwright),
        Some("log_pattern") => Some(VerificationMethod::LogPattern),
        Some("gui_automation") => Some(VerificationMethod::GuiAutomation),
        Some("type_check") => Some(VerificationMethod::TypeCheck),
        Some("lint_check") => Some(VerificationMethod::LintCheck),
        Some("custom_command") => Some(VerificationMethod::CustomCommand),
        _ => None,
    }
}

fn parse_worker_domains(value: &serde_json::Value) -> Option<Vec<WorkerDomain>> {
    let arr = value.as_array()?;
    let mut domains = Vec::new();

    for item in arr {
        let domain = WorkerDomain {
            worker_id: item["worker_id"].as_str()?.to_string(),
            specialization: item["specialization"].as_str().map(|s| s.to_string()),
            file_patterns: item["file_patterns"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default(),
            system_prompt_additions: item["system_prompt_additions"]
                .as_str()
                .map(|s| s.to_string()),
        };
        domains.push(domain);
    }

    if domains.is_empty() {
        None
    } else {
        Some(domains)
    }
}

/// Get the default list of available verification methods.
pub fn get_default_verification_methods(hints: &ProjectHints) -> Vec<VerificationMethodInfo> {
    vec![
        VerificationMethodInfo {
            method: VerificationMethod::BuildSuccess,
            description: "Verify the project builds without errors".to_string(),
            available: hints.has_npm || hints.has_cargo || hints.has_python,
            config_hints: Some("command: build command to run".to_string()),
        },
        VerificationMethodInfo {
            method: VerificationMethod::UnitTest,
            description: "Run unit tests and verify they pass".to_string(),
            available: !hints.test_frameworks.is_empty(),
            config_hints: Some("test_pattern: glob pattern for test files".to_string()),
        },
        VerificationMethodInfo {
            method: VerificationMethod::IntegrationTest,
            description: "Run integration tests".to_string(),
            available: !hints.test_frameworks.is_empty(),
            config_hints: Some("command: test command, test_pattern: optional filter".to_string()),
        },
        VerificationMethodInfo {
            method: VerificationMethod::Playwright,
            description: "Run Playwright browser tests".to_string(),
            available: hints.has_playwright,
            config_hints: Some("script_path or script_id".to_string()),
        },
        VerificationMethodInfo {
            method: VerificationMethod::TypeCheck,
            description: "Run type checker (tsc, mypy, etc.)".to_string(),
            available: hints.has_typescript || hints.has_python,
            config_hints: Some("command: type check command".to_string()),
        },
        VerificationMethodInfo {
            method: VerificationMethod::LintCheck,
            description: "Run linter and check for errors".to_string(),
            available: hints.has_npm || hints.has_python || hints.has_cargo,
            config_hints: Some("command: lint command".to_string()),
        },
        VerificationMethodInfo {
            method: VerificationMethod::LogPattern,
            description: "Check for patterns in log files".to_string(),
            available: true,
            config_hints: Some("pattern: regex, log_path: path, should_match: bool".to_string()),
        },
        VerificationMethodInfo {
            method: VerificationMethod::CustomCommand,
            description: "Run a custom shell command".to_string(),
            available: true,
            config_hints: Some("command: shell command to run".to_string()),
        },
    ]
}

/// Create a planning context from workspace information.
pub fn create_planning_context(
    goal: &str,
    workspace_root: &str,
    execution_steps: Option<serde_json::Value>,
    previous_plan: Option<VerificationPlan>,
    replan_reason: Option<String>,
) -> PlanningContext {
    let project_hints = ProjectHints::detect(workspace_root);
    let available_methods = get_default_verification_methods(&project_hints);

    info!(
        "Created planning context for goal: {} (npm: {}, cargo: {}, ts: {})",
        goal, project_hints.has_npm, project_hints.has_cargo, project_hints.has_typescript
    );

    PlanningContext {
        goal: goal.to_string(),
        available_methods,
        project_hints,
        configured_execution_steps: execution_steps,
        previous_plan,
        replan_reason,
    }
}

// ============================================================================
// Plan Creation and Storage
// ============================================================================

/// Result of plan creation.
#[derive(Debug, Clone)]
pub struct PlanCreationResult {
    pub plan: VerificationPlan,
    pub stored_plan_id: String,
    pub raw_ai_response: String,
}

/// Create a verification plan for a task.
///
/// This function:
/// 1. Builds the planning prompt from context
/// 2. Calls the AI provider to generate a plan
/// 3. Parses the AI response
/// 4. Stores the plan in the database
///
/// # Arguments
/// * `db` - Database handle for storing the plan
/// * `task_run_id` - The task run this plan is for
/// * `context` - Planning context with goal, project hints, etc.
/// * `timeout_seconds` - Timeout for AI call (0 = use default)
///
/// # Returns
/// `PlanCreationResult` with the plan and storage info, or error string
pub fn create_verification_plan(
    db: &crate::database::CheckpointDb,
    task_run_id: &str,
    context: &PlanningContext,
    timeout_seconds: u64,
) -> Result<PlanCreationResult, String> {
    use tracing::{debug, error};

    info!("Creating verification plan for task: {}", task_run_id);

    // Build the full prompt
    let system_prompt = build_planning_system_prompt();
    let user_prompt = build_planning_user_prompt(context);

    // Combine into a single prompt for the AI
    // (Most AI providers expect a single prompt, system instructions can be prepended)
    let full_prompt = format!(
        "{}\n\n---\n\n{}",
        system_prompt, user_prompt
    );

    debug!(
        "Planning prompt built (length: {} chars)",
        full_prompt.len()
    );

    // Build task context for routing (planning is typically medium/complex)
    let task_context = TaskContext::from_prompt(&full_prompt)
        .with_file_count(context.project_hints.test_frameworks.len());

    // Call the AI provider with routing
    let ai_response = crate::ai_provider::run_prompt_with_routing(&full_prompt, &task_context, timeout_seconds);

    if !ai_response.success {
        let error_msg = ai_response
            .error
            .unwrap_or_else(|| "Unknown AI error".to_string());
        error!("Planning AI call failed: {}", error_msg);
        return Err(format!("Planning AI call failed: {}", error_msg));
    }

    let raw_response = ai_response.output;
    debug!("Planning AI response (length: {} chars)", raw_response.len());

    // Parse the response into a VerificationPlan
    let mut plan = parse_planning_response(&raw_response)?;

    // Set the version based on whether this is a replan
    if context.previous_plan.is_some() {
        plan.version = context
            .previous_plan
            .as_ref()
            .map(|p| p.version + 1)
            .unwrap_or(1);
    }

    info!(
        "Parsed verification plan: {} criteria, {} AI-evaluated",
        plan.success_criteria.len(),
        plan.ai_criteria().len()
    );

    // Get previous version ID if this is a replan
    let previous_version_id = if context.previous_plan.is_some() {
        // Try to get the previous plan from the database
        db.get_latest_verification_plan(task_run_id)?
            .map(|p| p.id)
    } else {
        None
    };

    // Store in database
    let stored_plan = db.create_verification_plan(
        task_run_id,
        &plan,
        context.replan_reason.as_deref(),
        previous_version_id.as_deref(),
    )?;

    info!(
        "Stored verification plan {} (version {})",
        stored_plan.id, stored_plan.version
    );

    Ok(PlanCreationResult {
        plan,
        stored_plan_id: stored_plan.id,
        raw_ai_response: raw_response,
    })
}

/// Create a verification plan for replanning.
///
/// Similar to `create_verification_plan` but specifically for when a worker
/// requests a replan with a reason.
///
/// # Arguments
/// * `db` - Database handle
/// * `task_run_id` - The task run being replanned
/// * `workspace_root` - Path to workspace for project detection
/// * `replan_reason` - Why the replan was requested
/// * `timeout_seconds` - Timeout for AI call
pub fn create_replan(
    db: &crate::database::CheckpointDb,
    task_run_id: &str,
    workspace_root: &str,
    replan_reason: &str,
    timeout_seconds: u64,
) -> Result<PlanCreationResult, String> {
    info!(
        "Creating replan for task: {} (reason: {})",
        task_run_id, replan_reason
    );

    // Get the previous plan
    let previous_stored = db.get_latest_verification_plan(task_run_id)?
        .ok_or_else(|| "Cannot replan: no existing plan found".to_string())?;

    let previous_plan = previous_stored.parse_plan()?;
    let goal = previous_plan.goal_summary.clone(); // Clone before moving

    // Create context with replan info
    let context = create_planning_context(
        &goal, // Use original goal
        workspace_root,
        None, // No new execution steps
        Some(previous_plan),
        Some(replan_reason.to_string()),
    );

    create_verification_plan(db, task_run_id, &context, timeout_seconds)
}

/// Quick plan creation for simple tasks without full context.
///
/// This is a convenience function for creating a plan from just a goal string.
pub fn create_simple_plan(
    db: &crate::database::CheckpointDb,
    task_run_id: &str,
    goal: &str,
    workspace_root: &str,
    timeout_seconds: u64,
) -> Result<PlanCreationResult, String> {
    let context = create_planning_context(goal, workspace_root, None, None, None);
    create_verification_plan(db, task_run_id, &context, timeout_seconds)
}

// ============================================================================
// Worker Prompt Integration
// ============================================================================

/// Build a context section for workers that includes the verification plan.
///
/// This should be prepended to the worker's prompt to give them the plan context.
pub fn build_worker_plan_context(plan: &VerificationPlan) -> String {
    let mut context = String::new();

    context.push_str("## Verification Plan\n\n");
    context.push_str(&format!("**Goal:** {}\n\n", plan.goal_summary));

    context.push_str("### Success Criteria\n\n");
    context.push_str("Your work will be verified against these criteria. Focus on making them all pass.\n\n");

    for (i, criterion) in plan.success_criteria.iter().enumerate() {
        let critical_marker = if criterion.is_critical { " [CRITICAL]" } else { " [informational]" };
        context.push_str(&format!(
            "{}. **{}**{}: {}\n",
            i + 1, criterion.id, critical_marker, criterion.description
        ));

        match criterion.criterion_type {
            CriterionType::Deterministic => {
                if let Some(method) = &criterion.verification_method {
                    context.push_str(&format!("   - Verification: {:?}\n", method));
                }
                if let Some(config) = &criterion.verification_config {
                    if let Ok(config_str) = serde_json::to_string_pretty(config) {
                        if config_str.len() < 200 {
                            context.push_str(&format!("   - Config: {}\n", config_str.replace('\n', " ")));
                        }
                    }
                }
            }
            CriterionType::AiEvaluated => {
                if let Some(prompt) = &criterion.evaluation_prompt {
                    context.push_str(&format!("   - AI will check: {}\n", prompt));
                }
            }
        }
        context.push('\n');
    }

    context.push_str("### Worker Signals\n\n");
    context.push_str("When you believe the work is complete, emit:\n");
    context.push_str("```\n[WORK_COMPLETE] Brief reason why work is done\n```\n\n");
    context.push_str("The system will then run verification. If verification fails, you'll get feedback.\n\n");
    context.push_str("If you discover the plan is inadequate, emit:\n");
    context.push_str("```\n[NEED_REPLAN] Reason why replanning is needed\n```\n\n");

    context
}

/// Check if a prompt already contains a verification plan context.
pub fn prompt_has_plan_context(prompt: &str) -> bool {
    prompt.contains("## Verification Plan") && prompt.contains("### Success Criteria")
}

/// Inject verification plan context into an existing prompt.
///
/// If the prompt already has plan context, it's returned unchanged.
/// Otherwise, the plan context is prepended.
pub fn inject_plan_context(prompt: &str, plan: &VerificationPlan) -> String {
    if prompt_has_plan_context(prompt) {
        return prompt.to_string();
    }

    let plan_context = build_worker_plan_context(plan);
    format!("{}\n---\n\n{}", plan_context, prompt)
}

/// Get or create a verification plan for a task.
///
/// If a plan already exists for the task, returns it.
/// Otherwise, creates a new plan using the AI provider.
///
/// This is the main entry point for integrating planning into task execution.
pub fn ensure_verification_plan(
    db: &crate::database::CheckpointDb,
    task_run_id: &str,
    goal: &str,
    workspace_root: &str,
    timeout_seconds: u64,
) -> Result<VerificationPlan, String> {
    // Check if a plan already exists
    if let Some(stored_plan) = db.get_latest_verification_plan(task_run_id)? {
        info!(
            "Using existing verification plan {} (version {}) for task {}",
            stored_plan.id, stored_plan.version, task_run_id
        );
        return stored_plan.parse_plan();
    }

    // Create a new plan
    info!("Creating new verification plan for task {}", task_run_id);
    let result = create_simple_plan(db, task_run_id, goal, workspace_root, timeout_seconds)?;
    Ok(result.plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_planning_response() {
        let response = r#"
Here's the plan:

{
  "goal_summary": "Fix login validation",
  "success_criteria": [
    {
      "id": "build",
      "description": "Build passes",
      "type": "deterministic",
      "verification_method": "build_success",
      "required": true
    }
  ],
  "execution_steps": [],
  "suggested_worker_count": 1
}
"#;

        let plan = parse_planning_response(response).unwrap();
        assert_eq!(plan.goal_summary, "Fix login validation");
        assert_eq!(plan.success_criteria.len(), 1);
        assert_eq!(plan.success_criteria[0].id, "build");
    }

    #[test]
    fn test_parse_verification_method() {
        assert_eq!(
            parse_verification_method(Some("build_success")),
            Some(VerificationMethod::BuildSuccess)
        );
        assert_eq!(
            parse_verification_method(Some("unit_test")),
            Some(VerificationMethod::UnitTest)
        );
        assert_eq!(parse_verification_method(Some("invalid")), None);
        assert_eq!(parse_verification_method(None), None);
    }
}
