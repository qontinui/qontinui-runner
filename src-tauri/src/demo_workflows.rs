//! Demo workflow seeding for first-launch experience.
//!
//! On first launch (when no demo-category workflows exist), this module:
//! 1. Writes embedded Python setup scripts to the app data directory
//! 2. Creates 3 demo workflows that showcase the verification-agentic loop

use crate::database::CheckpointDb;
use crate::unified_workflows::CreateUnifiedWorkflowRequest;
use serde_json::json;
use tracing::{info, warn};

// Embed the Python setup scripts at compile time
const SETUP_CALCULATOR: &str = include_str!("../../examples/demo-workflows/setup_calculator.py");
const SETUP_TDD: &str = include_str!("../../examples/demo-workflows/setup_tdd.py");
const SETUP_DATA_PIPELINE: &str =
    include_str!("../../examples/demo-workflows/setup_data_pipeline.py");

/// Seed demo workflows if none with category "demo" exist.
pub fn seed_demo_workflows_if_needed(db: &CheckpointDb) {
    let conn = match db.get_conn() {
        Ok(c) => c,
        Err(e) => {
            warn!("Cannot seed demo workflows: {}", e);
            return;
        }
    };

    // Check if any demo workflows already exist
    let demo_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM unified_workflows WHERE category = 'demo'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);

    if demo_count > 0 {
        info!(
            "Demo workflows already exist ({} found), skipping seed",
            demo_count
        );
        return;
    }

    info!("No demo workflows found — seeding 3 demo workflows for first-launch experience");

    // Write setup scripts to app data directory
    let scripts_dir = match write_setup_scripts() {
        Ok(dir) => dir,
        Err(e) => {
            warn!("Failed to write demo setup scripts: {}", e);
            return;
        }
    };

    // Resolve workspace paths
    let temp_dir = std::env::temp_dir();
    let demo_base = temp_dir.join("qontinui-demo");

    let calculator_workspace = demo_base.join("calculator");
    let tdd_workspace = demo_base.join("string-utils");
    let pipeline_workspace = demo_base.join("data-pipeline");

    // Create the 3 demo workflows
    let workflows = vec![
        build_calculator_workflow(&scripts_dir, &calculator_workspace),
        build_tdd_workflow(&scripts_dir, &tdd_workspace),
        build_pipeline_workflow(&scripts_dir, &pipeline_workspace),
    ];

    let mut created = 0;
    for request in &workflows {
        match db.create_unified_workflow(request) {
            Ok(wf) => {
                info!("Seeded demo workflow: {} ({})", wf.name, wf.id);
                created += 1;
            }
            Err(e) => {
                warn!("Failed to seed demo workflow '{}': {}", request.name, e);
            }
        }
    }

    info!("Seeded {}/{} demo workflows", created, workflows.len());
}

/// Write embedded setup scripts to the app data directory.
/// Returns the path to the scripts directory.
fn write_setup_scripts() -> Result<String, String> {
    let app_data = dirs::data_dir()
        .ok_or_else(|| "Cannot determine app data directory".to_string())?
        .join("com.qontinui.runner")
        .join("demo-scripts");

    std::fs::create_dir_all(&app_data)
        .map_err(|e| format!("Cannot create demo-scripts directory: {}", e))?;

    let scripts = [
        ("setup_calculator.py", SETUP_CALCULATOR),
        ("setup_tdd.py", SETUP_TDD),
        ("setup_data_pipeline.py", SETUP_DATA_PIPELINE),
    ];

    for (name, content) in &scripts {
        let path = app_data.join(name);
        // Update workspace paths in the script to use the temp directory
        let updated_content = update_workspace_paths(content);
        std::fs::write(&path, updated_content)
            .map_err(|e| format!("Cannot write {}: {}", name, e))?;
    }

    let dir_str = app_data
        .to_str()
        .ok_or_else(|| "Demo scripts path contains invalid UTF-8".to_string())?
        .to_string();

    info!("Demo setup scripts written to: {}", dir_str);
    Ok(dir_str)
}

/// Update hardcoded workspace paths in setup scripts to use the system temp directory.
fn update_workspace_paths(script: &str) -> String {
    let temp_dir = std::env::temp_dir();
    let demo_base = temp_dir.join("qontinui-demo");
    let demo_base_str = demo_base.to_string_lossy().replace('\\', "/");

    // Replace the hardcoded path prefix with the resolved temp path
    script.replace("C:/temp/qontinui-demo", &demo_base_str)
}

fn build_calculator_workflow(
    scripts_dir: &str,
    workspace: &std::path::Path,
) -> CreateUnifiedWorkflowRequest {
    let workspace_str = workspace.to_string_lossy().replace('\\', "\\\\");
    let workspace_display = workspace.to_string_lossy().replace('\\', "/");

    CreateUnifiedWorkflowRequest {
        name: "Demo: Fix Buggy Calculator".to_string(),
        description: "A Python calculator has 3 bugs in its arithmetic functions (subtract, divide, modulo). The AI reads test failures and fixes the bugs until all tests pass. Quick demo (~1 iteration).".to_string(),
        category: "demo".to_string(),
        tags: vec![
            "demo".to_string(),
            "python".to_string(),
            "bug-fix".to_string(),
            "beginner".to_string(),
        ],
        setup_steps: vec![json!({
            "type": "command",
            "mode": "shell",
            "name": "Reset demo workspace",
            "phase": "setup",
            "shell_command": "python setup_calculator.py",
            "shell_command_working_directory": scripts_dir,
            "shell_command_fail_on_error": true
        })],
        verification_steps: vec![json!({
            "type": "command",
            "mode": "check",
            "name": "Run calculator tests",
            "phase": "verification",
            "check_type": "custom_command",
            "check_command": "python test_calculator.py",
            "check_working_directory": workspace_str
        })],
        agentic_steps: vec![json!({
            "type": "prompt",
            "name": "Fix calculator bugs",
            "phase": "agentic",
            "content": format!(
                "Fix the bugs in the Python calculator module so that all tests pass.\n\n\
                **Workspace:** {workspace_display}\n\
                **File to fix:** calculator.py\n\
                **Test file:** test_calculator.py (do NOT modify this file)\n\n\
                The test file is correct. Only modify calculator.py to fix the failing tests.\n\
                Read the test failures carefully - they tell you exactly which functions have bugs \
                and what the expected output should be."
            )
        })],
        completion_steps: vec![],
        max_iterations: 3,
        timeout_seconds: None,
        provider: None,
        model: None,
        skip_ai_summary: true,
        log_source_selection: None,
        context_ids: None,
        disabled_context_ids: None,
        auto_include_contexts: Some(false),
        prompt_template: None,
        log_watch_enabled: Some(false),
        health_check_enabled: Some(false),
        health_check_urls: None,
        preflight_check_enabled: Some(false),
        enable_sweep: Some(false),
        max_sweep_iterations: None,
        targeted_error_ids: None,
        generated_by_task_run_id: None,
        stages: None,
        stop_on_failure: Some(false),
        constraint_overrides: None,
        approval_gate: Some(false),
        reflection_mode: Some(false),
        completion_prompts_first: None,
        model_overrides: None,
        dependency_graph: None,
        cost_annotations: None,
        quality_report: None,
        acceptance_criteria: None,
        ai_reviewed: None,
        workflow_architecture: None,
        enforce_token_budget: None,
        strict_cwd: false,
        tool_tags: Vec::new(),
        flow_control_json: None,
        phase_timeouts_json: None,
        rollback_policy: None,
    }
}

fn build_tdd_workflow(
    scripts_dir: &str,
    workspace: &std::path::Path,
) -> CreateUnifiedWorkflowRequest {
    let workspace_str = workspace.to_string_lossy().replace('\\', "\\\\");
    let workspace_display = workspace.to_string_lossy().replace('\\', "/");

    CreateUnifiedWorkflowRequest {
        name: "Demo: Implement from Tests (TDD)".to_string(),
        description: "Test-driven development demo. A test file defines 5 string utility functions that don't exist yet. The AI must implement all functions from scratch to make the tests pass.".to_string(),
        category: "demo".to_string(),
        tags: vec![
            "demo".to_string(),
            "python".to_string(),
            "tdd".to_string(),
            "implementation".to_string(),
        ],
        setup_steps: vec![json!({
            "type": "command",
            "mode": "shell",
            "name": "Reset demo workspace",
            "phase": "setup",
            "shell_command": "python setup_tdd.py",
            "shell_command_working_directory": scripts_dir,
            "shell_command_fail_on_error": true
        })],
        verification_steps: vec![json!({
            "type": "command",
            "mode": "check",
            "name": "Run string utils tests",
            "phase": "verification",
            "check_type": "custom_command",
            "check_command": "python test_string_utils.py",
            "check_working_directory": workspace_str
        })],
        agentic_steps: vec![json!({
            "type": "prompt",
            "name": "Implement string utils",
            "phase": "agentic",
            "content": format!(
                "Implement the missing string utility functions so that all tests pass.\n\n\
                **Workspace:** {workspace_display}\n\
                **File to implement:** string_utils.py (currently empty - you need to write all 5 functions)\n\
                **Test file:** test_string_utils.py (do NOT modify this file)\n\n\
                Read the test file to understand what each function should do:\n\
                - reverse_words(s): reverse the order of words in a string\n\
                - title_case(s): capitalize first letter of each word, lowercase the rest\n\
                - count_vowels(s): count vowels (a,e,i,o,u) case-insensitive\n\
                - is_palindrome(s): check palindrome ignoring case and spaces\n\
                - truncate(s, max_length): truncate to max_length with \"...\" suffix if truncated"
            )
        })],
        completion_steps: vec![],
        max_iterations: 3,
        timeout_seconds: None,
        provider: None,
        model: None,
        skip_ai_summary: true,
        log_source_selection: None,
        context_ids: None,
        disabled_context_ids: None,
        auto_include_contexts: Some(false),
        prompt_template: None,
        log_watch_enabled: Some(false),
        health_check_enabled: Some(false),
        health_check_urls: None,
        preflight_check_enabled: Some(false),
        enable_sweep: Some(false),
        max_sweep_iterations: None,
        targeted_error_ids: None,
        generated_by_task_run_id: None,
        stages: None,
        stop_on_failure: Some(false),
        constraint_overrides: None,
        approval_gate: Some(false),
        reflection_mode: Some(false),
        completion_prompts_first: None,
        model_overrides: None,
        dependency_graph: None,
        cost_annotations: None,
        quality_report: None,
        acceptance_criteria: None,
        ai_reviewed: None,
        workflow_architecture: None,
        enforce_token_budget: None,
        strict_cwd: false,
        tool_tags: Vec::new(),
        flow_control_json: None,
        phase_timeouts_json: None,
        rollback_policy: None,
    }
}

fn build_pipeline_workflow(
    scripts_dir: &str,
    workspace: &std::path::Path,
) -> CreateUnifiedWorkflowRequest {
    let workspace_str = workspace.to_string_lossy().replace('\\', "\\\\");
    let workspace_display = workspace.to_string_lossy().replace('\\', "/");

    CreateUnifiedWorkflowRequest {
        name: "Demo: Fix Data Pipeline".to_string(),
        description: "A sales data processing pipeline has bugs in its revenue calculation and aggregation logic. The AI must fix the pipeline so the output matches expected values. Shows real-world data processing debugging.".to_string(),
        category: "demo".to_string(),
        tags: vec![
            "demo".to_string(),
            "python".to_string(),
            "data".to_string(),
            "pipeline".to_string(),
            "debugging".to_string(),
        ],
        setup_steps: vec![json!({
            "type": "command",
            "mode": "shell",
            "name": "Reset demo workspace",
            "phase": "setup",
            "shell_command": "python setup_data_pipeline.py",
            "shell_command_working_directory": scripts_dir,
            "shell_command_fail_on_error": true
        })],
        verification_steps: vec![json!({
            "type": "command",
            "mode": "check",
            "name": "Validate pipeline output",
            "phase": "verification",
            "check_type": "custom_command",
            "check_command": "python validate_pipeline.py",
            "check_working_directory": workspace_str
        })],
        agentic_steps: vec![json!({
            "type": "prompt",
            "name": "Fix pipeline bugs",
            "phase": "agentic",
            "content": format!(
                "Fix the bugs in the data processing pipeline so that all validation checks pass.\n\n\
                **Workspace:** {workspace_display}\n\
                **File to fix:** pipeline.py (the data processing pipeline)\n\
                **Input data:** sales_data.csv (do NOT modify)\n\
                **Validation script:** validate_pipeline.py (do NOT modify)\n\n\
                The pipeline reads sales_data.csv, calculates revenue per row, aggregates by product \
                and region, and outputs a JSON report. The validation script checks the report against \
                manually calculated expected values.\n\n\
                Focus on: the calculate_revenue function has a bug with type conversion and arithmetic, \
                and find_top_product returns the wrong result."
            )
        })],
        completion_steps: vec![],
        max_iterations: 5,
        timeout_seconds: None,
        provider: None,
        model: None,
        skip_ai_summary: true,
        log_source_selection: None,
        context_ids: None,
        disabled_context_ids: None,
        auto_include_contexts: Some(false),
        prompt_template: None,
        log_watch_enabled: Some(false),
        health_check_enabled: Some(false),
        health_check_urls: None,
        preflight_check_enabled: Some(false),
        enable_sweep: Some(false),
        max_sweep_iterations: None,
        targeted_error_ids: None,
        generated_by_task_run_id: None,
        stages: None,
        stop_on_failure: Some(false),
        constraint_overrides: None,
        approval_gate: Some(false),
        reflection_mode: Some(false),
        completion_prompts_first: None,
        model_overrides: None,
        dependency_graph: None,
        cost_annotations: None,
        quality_report: None,
        acceptance_criteria: None,
        ai_reviewed: None,
        workflow_architecture: None,
        enforce_token_budget: None,
        strict_cwd: false,
        tool_tags: Vec::new(),
        flow_control_json: None,
        phase_timeouts_json: None,
        rollback_policy: None,
    }
}
