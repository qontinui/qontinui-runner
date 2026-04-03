//! Concrete Role Specializations
//!
//! Defines four specialized agent roles with restricted tool whitelists and
//! role-specific system prompts. These roles follow the Analyze-Plan-Execute-Verify
//! pattern used in agentic workflows.
//!
//! Each specialization sets:
//! - A descriptive role, goal, and backstory
//! - An `allowed_tools` whitelist restricting which tools the role may invoke
//! - A `preferred_model_tier` guiding model selection
//! - A `max_tokens_budget` bounding response length
//!
//! ## Tool Name Format
//!
//! Tool names correspond to the MCP tool names registered in the Python MCP server
//! (`qontinui-mcp/src/qontinui_mcp/server.py`). These are snake_case identifiers
//! like `get_task_runs`, `run_workflow`, etc. When invoked via Claude Code, they
//! appear as `mcp__qontinui__<tool_name>`.
//!
//! ## ToolGuard Integration
//!
//! The `ToolGuard` is created in `flow_executor.rs` from these whitelists and
//! enforced in `execute_tool_step`. The guard's `check_tool()` method is called
//! with the bare tool name (without the `mcp__qontinui__` prefix) before each
//! tool invocation, blocking tools not in the role's whitelist.

#![allow(dead_code)]

use super::agent_roles::{AgentRole, AutonomyLevel, CommunicationStyle, ModelTier, RoleRegistry};

// ============================================================================
// Analyzer
// ============================================================================

/// Create an Analyzer role — inspects and understands current state without
/// modifying anything.
///
/// Whitelisted tools are read-only MCP endpoints for querying task runs,
/// automation history, screenshots, DOM captures, state machine status, and logs.
pub fn analyzer_role() -> AgentRole {
    AgentRole::new(
        "analyzer",
        "Analyzer",
        "Inspect and understand the current state",
    )
    .with_backstory(
        "You are an expert at reading code, logs, and state without modifying anything. \
             You gather evidence, identify patterns, and produce clear summaries so that \
             downstream roles can act on solid information.",
    )
    .with_allowed_tools(vec![
        // Task run queries
        "get_task_runs".to_string(),
        "get_task_run".to_string(),
        "get_task_run_events".to_string(),
        "get_task_run_screenshots".to_string(),
        "get_task_run_playwright_results".to_string(),
        // Automation run queries
        "get_automation_runs".to_string(),
        "get_automation_run".to_string(),
        // Config queries
        "get_executor_status".to_string(),
        "get_loaded_config".to_string(),
        "list_monitors".to_string(),
        // Screenshot & log queries
        "list_screenshots".to_string(),
        "read_runner_logs".to_string(),
        "get_annotated_screenshot".to_string(),
        // DOM capture queries
        "list_dom_captures".to_string(),
        "get_dom_capture".to_string(),
        "get_dom_capture_html".to_string(),
        // State machine queries
        "get_state_machine_status".to_string(),
        "get_active_states".to_string(),
        "get_available_transitions".to_string(),
        // Test queries (read-only)
        "list_tests".to_string(),
        "get_test".to_string(),
        "list_test_results".to_string(),
        "get_test_history".to_string(),
        // AWAS queries
        "awas_list_actions".to_string(),
        "awas_check_support".to_string(),
    ])
    .with_model_tier(ModelTier::Simple)
    .with_max_tokens_budget(2000)
    .with_style(CommunicationStyle::Concise)
    .with_autonomy(AutonomyLevel::Autonomous)
    .with_constraint("Never modify state — read-only operations only.".to_string())
    .with_constraint("Summarize findings with supporting evidence.".to_string())
    .with_tag("analysis")
    .with_tag("read-only")
}

// ============================================================================
// Planner
// ============================================================================

/// Create a Planner role — designs approaches and strategies by analyzing
/// patterns and evidence.
///
/// Has all Analyzer tools plus comparison, visual diff, AI generation,
/// and interaction heatmap tools for deeper analysis.
pub fn planner_role() -> AgentRole {
    AgentRole::new("planner", "Planner", "Design the approach and strategy")
        .with_backstory(
            "You are an architect who designs solutions by analyzing patterns and evidence. \
             You consider trade-offs, outline clear steps, and anticipate failure modes. \
             Your plans are actionable and specific enough for an executor to follow.",
        )
        .with_allowed_tools(vec![
            // All Analyzer read-only tools
            "get_task_runs".to_string(),
            "get_task_run".to_string(),
            "get_task_run_events".to_string(),
            "get_task_run_screenshots".to_string(),
            "get_task_run_playwright_results".to_string(),
            "get_automation_runs".to_string(),
            "get_automation_run".to_string(),
            "get_executor_status".to_string(),
            "get_loaded_config".to_string(),
            "list_monitors".to_string(),
            "list_screenshots".to_string(),
            "read_runner_logs".to_string(),
            "get_annotated_screenshot".to_string(),
            "list_dom_captures".to_string(),
            "get_dom_capture".to_string(),
            "get_dom_capture_html".to_string(),
            "get_state_machine_status".to_string(),
            "get_active_states".to_string(),
            "get_available_transitions".to_string(),
            "list_tests".to_string(),
            "get_test".to_string(),
            "list_test_results".to_string(),
            "get_test_history".to_string(),
            "awas_list_actions".to_string(),
            "awas_check_support".to_string(),
            // Visual comparison & analysis tools
            "get_visual_diff".to_string(),
            "get_interaction_heatmap".to_string(),
            // AI generation / workflow planning
            "generate_workflow".to_string(),
        ])
        .with_model_tier(ModelTier::Complex)
        .with_max_tokens_budget(4000)
        .with_style(CommunicationStyle::Detailed)
        .with_autonomy(AutonomyLevel::Guided)
        .with_constraint("Produce a numbered step-by-step plan.".to_string())
        .with_constraint("Consider at least one alternative approach.".to_string())
        .with_tag("planning")
        .with_tag("strategy")
}

// ============================================================================
// Executor
// ============================================================================

/// Create an Executor role — carries out planned actions precisely.
///
/// Has write/execution tools: workflow execution, Python execution,
/// state transitions, AWAS automation, GUI capture, config loading,
/// and sub-agent spawning.
pub fn executor_role() -> AgentRole {
    AgentRole::new("executor", "Executor", "Execute actions and make changes")
        .with_backstory(
            "You are a skilled operator who executes planned actions precisely. You follow \
             the plan step by step, report progress, and stop immediately if something \
             unexpected happens. You never improvise beyond the plan without escalating.",
        )
        .with_allowed_tools(vec![
            // Workflow execution
            "run_workflow".to_string(),
            "stop_execution".to_string(),
            "execute_plan".to_string(),
            // Config loading
            "load_config".to_string(),
            "ensure_config_loaded".to_string(),
            // Python / script execution
            "execute_python".to_string(),
            // State machine execution
            "load_state_machine".to_string(),
            "execute_state_transition".to_string(),
            "navigate_to_states".to_string(),
            // AWAS automation
            "awas_discover".to_string(),
            "awas_execute".to_string(),
            // GUI config capture & build
            "capture_gui_elements".to_string(),
            "capture_multi_state_gui_config".to_string(),
            "build_gui_config".to_string(),
            // Sub-agent orchestration
            "spawn_sub_agent".to_string(),
            // Test CRUD (executor can create/update tests as part of plan)
            "create_test".to_string(),
            "update_test".to_string(),
            "delete_test".to_string(),
            // Log migration
            "migrate_task_run_logs".to_string(),
        ])
        .with_model_tier(ModelTier::Medium)
        .with_max_tokens_budget(4000)
        .with_style(CommunicationStyle::Technical)
        .with_autonomy(AutonomyLevel::Guided)
        .with_constraint("Follow the provided plan exactly.".to_string())
        .with_constraint("Report each action and its result.".to_string())
        .with_constraint("Stop and escalate on unexpected errors.".to_string())
        .with_tag("execution")
        .with_tag("write")
}

// ============================================================================
// Verifier
// ============================================================================

/// Create a Verifier role — validates results and verifies correctness.
///
/// Has read tools for inspecting results plus test execution, visual diff,
/// and state verification tools.
pub fn verifier_role() -> AgentRole {
    AgentRole::new(
        "verifier",
        "Verifier",
        "Validate results and verify correctness",
    )
    .with_backstory(
        "You are a QA specialist who verifies outcomes match expectations. You run \
             tests, compare before/after state, and produce a clear pass/fail verdict \
             with evidence. You are skeptical by nature and look for subtle regressions.",
    )
    .with_allowed_tools(vec![
        // Task run inspection
        "get_task_runs".to_string(),
        "get_task_run".to_string(),
        "get_task_run_events".to_string(),
        "get_task_run_screenshots".to_string(),
        "get_task_run_playwright_results".to_string(),
        // Test execution & results
        "execute_test".to_string(),
        "list_tests".to_string(),
        "get_test".to_string(),
        "list_test_results".to_string(),
        "get_test_history".to_string(),
        // Visual verification
        "get_annotated_screenshot".to_string(),
        "get_visual_diff".to_string(),
        "list_screenshots".to_string(),
        // DOM state verification
        "list_dom_captures".to_string(),
        "get_dom_capture".to_string(),
        "get_dom_capture_html".to_string(),
        // State machine verification
        "get_state_machine_status".to_string(),
        "get_active_states".to_string(),
        "get_available_transitions".to_string(),
        // Automation run results
        "get_automation_runs".to_string(),
        "get_automation_run".to_string(),
        // Logs for evidence
        "read_runner_logs".to_string(),
    ])
    .with_model_tier(ModelTier::Medium)
    .with_max_tokens_budget(2000)
    .with_style(CommunicationStyle::Technical)
    .with_autonomy(AutonomyLevel::Autonomous)
    .with_constraint("Always produce an explicit pass/fail verdict.".to_string())
    .with_constraint("Include evidence for every verdict.".to_string())
    .with_tag("verification")
    .with_tag("qa")
}

// ============================================================================
// Registration
// ============================================================================

/// Register all four default specialization roles in the given registry.
pub fn register_default_specializations(registry: &mut RoleRegistry) {
    registry.register(analyzer_role());
    registry.register(planner_role());
    registry.register(executor_role());
    registry.register(verifier_role());
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_analyzer_role() {
        let role = analyzer_role();
        assert_eq!(role.id, "analyzer");
        assert_eq!(role.preferred_model_tier, ModelTier::Simple);
        assert_eq!(role.max_tokens_budget, Some(2000));
        // Verify actual MCP tool names are present
        let tools = role.allowed_tools.as_ref().unwrap();
        assert!(tools.contains(&"get_task_runs".to_string()));
        assert!(tools.contains(&"get_task_run".to_string()));
        assert!(tools.contains(&"get_dom_capture".to_string()));
        assert!(tools.contains(&"get_state_machine_status".to_string()));
        assert!(tools.contains(&"read_runner_logs".to_string()));
    }

    #[test]
    fn test_planner_role() {
        let role = planner_role();
        assert_eq!(role.id, "planner");
        assert_eq!(role.preferred_model_tier, ModelTier::Complex);
        assert_eq!(role.max_tokens_budget, Some(4000));
        let tools = role.allowed_tools.as_ref().unwrap();
        // Planner has analyzer tools plus comparison/generation
        assert!(tools.contains(&"get_task_runs".to_string()));
        assert!(tools.contains(&"get_visual_diff".to_string()));
        assert!(tools.contains(&"generate_workflow".to_string()));
    }

    #[test]
    fn test_executor_role() {
        let role = executor_role();
        assert_eq!(role.id, "executor");
        assert_eq!(role.preferred_model_tier, ModelTier::Medium);
        let tools = role.allowed_tools.as_ref().unwrap();
        assert!(tools.contains(&"run_workflow".to_string()));
        assert!(tools.contains(&"execute_python".to_string()));
        assert!(tools.contains(&"execute_plan".to_string()));
        assert!(tools.contains(&"awas_execute".to_string()));
    }

    #[test]
    fn test_verifier_role() {
        let role = verifier_role();
        assert_eq!(role.id, "verifier");
        assert_eq!(role.preferred_model_tier, ModelTier::Medium);
        let tools = role.allowed_tools.as_ref().unwrap();
        assert!(tools.contains(&"execute_test".to_string()));
        assert!(tools.contains(&"get_visual_diff".to_string()));
        assert!(tools.contains(&"list_test_results".to_string()));
        assert!(tools.contains(&"get_task_run_playwright_results".to_string()));
    }

    #[test]
    fn test_register_default_specializations() {
        let mut registry = RoleRegistry::new();
        register_default_specializations(&mut registry);

        assert!(registry.get("analyzer").is_some());
        assert!(registry.get("planner").is_some());
        assert!(registry.get("executor").is_some());
        assert!(registry.get("verifier").is_some());
    }

    #[test]
    fn test_specializations_build_system_prompts() {
        // Ensure all four roles produce valid system prompts without panicking
        for role in &[
            analyzer_role(),
            planner_role(),
            executor_role(),
            verifier_role(),
        ] {
            let prompt = role.build_system_prompt();
            assert!(!prompt.is_empty());
            assert!(prompt.contains(&role.role));
        }
    }

    #[test]
    fn test_no_duplicate_tools_in_roles() {
        // Verify no role has duplicate tool entries
        for role in &[
            analyzer_role(),
            planner_role(),
            executor_role(),
            verifier_role(),
        ] {
            let tools = role.allowed_tools.as_ref().unwrap();
            let mut seen = std::collections::HashSet::new();
            for tool in tools {
                assert!(
                    seen.insert(tool.clone()),
                    "Duplicate tool '{}' in role '{}'",
                    tool,
                    role.id
                );
            }
        }
    }

    #[test]
    fn test_executor_has_no_read_only_overlap() {
        // Executor should focus on write/execute tools, not query tools
        let executor = executor_role();
        let tools = executor.allowed_tools.as_ref().unwrap();
        // These read-only tools should NOT be in executor
        assert!(!tools.contains(&"get_task_runs".to_string()));
        assert!(!tools.contains(&"list_screenshots".to_string()));
        assert!(!tools.contains(&"read_runner_logs".to_string()));
    }
}
