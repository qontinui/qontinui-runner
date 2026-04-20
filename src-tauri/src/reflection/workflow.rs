//! Programmatic reflection workflow definition.
//!
//! Builds the execution steps for a reflection workflow, including:
//! - Setup phase: Load source run data via curl commands
//! - Agentic phase: AI analyzes the data and applies fixes
//! - Verification phase: Verify no destructive changes
//! - Completion phase: Record patterns and evaluate effectiveness

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;
use tracing::info;

use crate::orchestrator::context_propagation::SharedVariableStore;
use crate::step_executor::ExecutionStepConfig;
use crate::unified_workflow_executor::LoopConfig;

// =============================================================================
// Shared Reflection Guardrails
// =============================================================================

/// Guardrail text included in ALL reflection system prompts.
/// Prevents scope creep, phantom file creation, and destructive modifications.
const REFLECTION_GUARDRAILS: &str = r#"
## Reflection Guardrails (applies to ALL reflection types)

### Scope Discipline
- **Stay in your lane.** Each reflection type has a specific domain (execution analysis, generation quality,
  project learning, UI Bridge improvements). Do NOT fix issues outside your domain. If you discover an
  issue in another domain, mention it in your output but do NOT attempt to fix it.
- **Analyze what happened, then fix within scope.** Your primary job is analysis. Fixes are secondary
  and must be within your reflection type's scope.

### Modification Boundaries
- **Never create new files or directories** unless the fix absolutely requires a new file (e.g., a new
  test file). Compatibility shims, re-export modules, and wrapper files are NEVER acceptable — fix the
  reference at the source instead.
- **Never modify code unrelated to your findings.** If a finding leads you to discover a separate issue
  in unrelated code, record it as a finding but do not modify it.
- **Prefer minimal, targeted changes.** A one-line fix is better than a refactor. Change the smallest
  amount of code that addresses the root cause.

### Path and Reference Verification
- **Verify before referencing.** Before citing a file path, module, function, or variable in a fix,
  verify it exists using the project structure or a targeted grep. Do NOT assume paths from memory,
  documentation, or naming conventions.
- **Fix the reference, not the filesystem.** If verification code, documentation, or a configuration
  references a wrong path, update the reference to point to the correct location. Do NOT create files
  at the wrong path to satisfy the broken reference.

### Change Safety
- **Never delete or overwrite user code** without clear evidence it is the root cause of the issue.
- **Test compilation after code changes.** If you modify Rust code, run `cargo check`. If you modify
  TypeScript, run `npx tsc --noEmit`. Do not emit a fix marker for code that doesn't compile.
- **Do not modify database schemas, migrations, or seed data** unless the reflection is specifically
  about a schema issue with clear evidence.
"#;

// =============================================================================
// Reflection Context Enrichment
// =============================================================================

/// Regex for extracting file paths from findings/output text.
fn file_path_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // Match patterns like:
        // - File: path/to/file.rs
        // - src/foo/bar.ts
        // - paths ending in common extensions
        Regex::new(
            r#"(?:File:\s*)?(?:^|\s|`|'|")((?:[a-zA-Z]:[/\\])?(?:[\w.@-]+[/\\])*[\w.@-]+\.(?:rs|ts|tsx|js|jsx|py|json|toml|yaml|yml|md|css|scss|html|vue|svelte))\b"#
        ).unwrap()
    })
}

/// Enrich shared variables with project root and file tree only.
///
/// This is a lightweight version of `enrich_reflection_context` that ONLY sets
/// `project_root` and `project_structure` if they aren't already set. It does
/// NOT extract referenced files or pre-read file contents.
///
/// Use this for all workflows that need basic project orientation (paths, tree)
/// without the heavier reflection-specific enrichment.
pub fn enrich_project_context(
    shared_vars: &SharedVariableStore,
    project_path: Option<&str>,
    app_state: &crate::commands::AppState,
) {
    // Skip if already set
    if shared_vars.get("project_root").is_some() {
        return;
    }

    info!("Enriching project context with root and file tree");

    let project_root = determine_project_root(shared_vars, project_path);
    shared_vars.set(
        "project_root",
        project_root
            .as_deref()
            .unwrap_or("Unknown - no project path available")
            .to_string(),
    );

    if let Some(ref root) = project_root {
        let tree = generate_file_tree(root);
        if tree.is_empty() {
            shared_vars.set(
                "project_structure",
                format!("Could not read directory tree for: {}", root),
            );
        } else {
            shared_vars.set("project_structure", tree);
        }
    } else {
        shared_vars.set(
            "project_structure",
            "No project path available — cannot generate directory tree.".to_string(),
        );
    }

    // Set runner API URL so prompts don't hardcode port 9876
    if shared_vars.get("runner_api_url").is_none() {
        shared_vars.set(
            "runner_api_url",
            crate::mcp::types::get_self_base_url(app_state),
        );
    }
}

/// Enrich shared variables with pre-loaded file contents and project structure
/// for reflection workflows. This makes reflection dramatically more efficient
/// by pre-loading everything the AI needs.
///
/// Sets these variables in the shared store:
/// - `referenced_files` - Newline-separated list of validated file paths
/// - `referenced_file_contents` - Pre-read contents of referenced files
/// - `project_structure` - Condensed directory tree
/// - `project_root` - The project root directory
pub fn enrich_reflection_context(shared_vars: &SharedVariableStore, project_path: Option<&str>) {
    info!("Enriching reflection context with pre-loaded files and project structure");

    // Determine project root
    let project_root = determine_project_root(shared_vars, project_path);
    shared_vars.set(
        "project_root",
        project_root
            .as_deref()
            .unwrap_or("Unknown - no project path available")
            .to_string(),
    );
    info!(
        "  project_root = {:?}",
        project_root.as_deref().unwrap_or("(none)")
    );

    // A. Extract file paths from findings/output
    let referenced_paths = extract_referenced_files(shared_vars, project_root.as_deref());
    if referenced_paths.is_empty() {
        shared_vars.set(
            "referenced_files",
            "No referenced files found in findings or AI output.".to_string(),
        );
        shared_vars.set(
            "referenced_file_contents",
            "No referenced files to pre-load.".to_string(),
        );
    } else {
        info!("  Found {} referenced files", referenced_paths.len());
        shared_vars.set(
            "referenced_files",
            referenced_paths
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join("\n"),
        );

        // C. Pre-read referenced files
        let contents = pre_read_files(&referenced_paths);
        if contents.is_empty() {
            shared_vars.set(
                "referenced_file_contents",
                "Could not read any of the referenced files.".to_string(),
            );
        } else {
            shared_vars.set("referenced_file_contents", contents);
        }
    }

    // B. Generate scoped file tree
    if let Some(ref root) = project_root {
        let tree = generate_file_tree(root);
        if tree.is_empty() {
            shared_vars.set(
                "project_structure",
                format!("Could not read directory tree for: {}", root),
            );
        } else {
            shared_vars.set("project_structure", tree);
        }
    } else {
        shared_vars.set(
            "project_structure",
            "No project path available — cannot generate directory tree.".to_string(),
        );
    }
}

/// Determine the project root from explicit path or workflow state.
fn determine_project_root(
    shared_vars: &SharedVariableStore,
    project_path: Option<&str>,
) -> Option<String> {
    // Prefer explicit project_path
    if let Some(p) = project_path {
        if !p.is_empty() {
            return Some(p.to_string());
        }
    }

    // Fall back: extract from source_workflow_state (look for working_directory)
    if let Some(state) = shared_vars.get("source_workflow_state") {
        // Try JSON parse
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&state) {
            if let Some(wd) = val.get("working_directory").and_then(|v| v.as_str()) {
                if !wd.is_empty() {
                    return Some(wd.to_string());
                }
            }
            if let Some(wd) = val.get("project_path").and_then(|v| v.as_str()) {
                if !wd.is_empty() {
                    return Some(wd.to_string());
                }
            }
        }
        // Try plain text pattern: working_directory: /path or "working_directory":"/path"
        static WD_RE: OnceLock<Regex> = OnceLock::new();
        let wd_re = WD_RE.get_or_init(|| {
            Regex::new(r#"(?:working_directory|project_path)["\s:]+["']?([^\s"',}]+)"#).unwrap()
        });
        if let Some(cap) = wd_re.captures(&state) {
            let wd = cap.get(1).unwrap().as_str();
            if !wd.is_empty() {
                return Some(wd.to_string());
            }
        }
    }

    None
}

/// Extract file paths from source_findings and source_ai_output, validate they exist.
fn extract_referenced_files(
    shared_vars: &SharedVariableStore,
    project_root: Option<&str>,
) -> Vec<PathBuf> {
    let re = file_path_regex();
    let mut seen = HashSet::new();
    let mut valid_paths = Vec::new();

    // Gather text to search
    let mut texts = Vec::new();
    if let Some(findings) = shared_vars.get("source_findings") {
        texts.push(findings);
    }
    if let Some(output) = shared_vars.get("source_ai_output") {
        texts.push(output);
    }
    if let Some(events) = shared_vars.get("source_pipeline_events") {
        texts.push(events);
    }

    for text in &texts {
        for cap in re.captures_iter(text) {
            let raw_path = cap.get(1).unwrap().as_str().to_string();
            if seen.contains(&raw_path) {
                continue;
            }
            seen.insert(raw_path.clone());

            // Try to resolve the path
            if let Some(resolved) = resolve_file_path(&raw_path, project_root) {
                valid_paths.push(resolved);
            }
        }
    }

    valid_paths
}

/// Try to resolve a file path: project_root + relative, then absolute.
fn resolve_file_path(raw_path: &str, project_root: Option<&str>) -> Option<PathBuf> {
    // Try relative to project root first
    if let Some(root) = project_root {
        let candidate = Path::new(root).join(raw_path);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    // Try as absolute path
    let abs = Path::new(raw_path);
    if abs.is_absolute() && abs.is_file() {
        return Some(abs.to_path_buf());
    }

    None
}

/// Pre-read up to 10 files, max 300 lines each, max 50KB total.
fn pre_read_files(paths: &[PathBuf]) -> String {
    let mut result = String::new();
    let mut total_bytes = 0usize;
    let max_total_bytes = 50 * 1024; // 50KB
    let max_files = 10;
    let max_lines = 300;

    for (i, path) in paths.iter().enumerate() {
        if i >= max_files {
            break;
        }
        if total_bytes >= max_total_bytes {
            break;
        }

        match std::fs::read_to_string(path) {
            Ok(content) => {
                // Limit to max_lines lines
                let truncated: String = content
                    .lines()
                    .take(max_lines)
                    .collect::<Vec<_>>()
                    .join("\n");

                // Check total size budget
                let remaining = max_total_bytes.saturating_sub(total_bytes);
                let portion = if truncated.len() > remaining {
                    &truncated[..remaining]
                } else {
                    &truncated
                };

                result.push_str(&format!(
                    "--- File: {} ---\n{}\n\n",
                    path.display(),
                    portion
                ));
                total_bytes += portion.len();

                let was_truncated =
                    truncated.len() < content.len() || portion.len() < truncated.len();
                if was_truncated {
                    result.push_str("(truncated)\n\n");
                }
            }
            Err(e) => {
                info!("  Could not read {}: {}", path.display(), e);
            }
        }
    }

    result
}

/// Generate a condensed indented file tree for the project.
/// Depth limit: 4, max entries: 500.
pub fn generate_file_tree(root: &str) -> String {
    let root_path = Path::new(root);
    if !root_path.is_dir() {
        return String::new();
    }

    let mut lines = Vec::new();
    let root_name = root_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| root.to_string());
    lines.push(format!("{}/", root_name));

    walk_dir_tree(root_path, 1, 4, &mut lines, 500);

    if lines.len() >= 500 {
        lines.push("... (truncated at 500 entries)".to_string());
    }

    lines.join("\n")
}

/// Directories to exclude from the file tree.
const EXCLUDED_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "__pycache__",
    ".next",
    "dist",
    "build",
    ".dev-logs",
    ".cache",
    ".turbo",
];

/// Recursively walk the directory tree with depth and entry limits.
fn walk_dir_tree(
    dir: &Path,
    depth: usize,
    max_depth: usize,
    lines: &mut Vec<String>,
    max_entries: usize,
) {
    if depth > max_depth || lines.len() >= max_entries {
        return;
    }

    let mut entries: Vec<_> = match std::fs::read_dir(dir) {
        Ok(rd) => rd.filter_map(|e| e.ok()).collect(),
        Err(_) => return,
    };

    // Sort entries for deterministic output
    entries.sort_by_key(|e| e.file_name());

    let indent = "  ".repeat(depth);

    for entry in entries {
        if lines.len() >= max_entries {
            return;
        }

        let name = entry.file_name().to_string_lossy().to_string();
        let path = entry.path();

        if path.is_dir() {
            if EXCLUDED_DIRS.contains(&name.as_str()) {
                continue;
            }
            lines.push(format!("{}{}/", indent, name));
            walk_dir_tree(&path, depth + 1, max_depth, lines, max_entries);
        } else {
            lines.push(format!("{}{}", indent, name));
        }
    }
}

/// Build the LoopConfig for a reflection workflow.
pub fn build_reflection_config(
    execution_id: &str,
    workflow_name: &str,
    source_workflow_name: &str,
) -> LoopConfig {
    LoopConfig {
        max_iterations: 3,
        base_prompt: build_agentic_prompt(source_workflow_name),
        workflow_name: workflow_name.to_string(),
        workflow_id: format!("reflection-{}", execution_id),
        execution_id: execution_id.to_string(),
        targeted_error_ids: Vec::new(),
        starting_iteration: 0,
        run_agentic_first: true, // Run analysis before verification
        artifact_dir: None,
        is_dev_mode: false, // CRITICAL: Prevents cascade reflection
        enable_sweep: false,
        max_sweep_iterations: 5,
        stages: Vec::new(),
        stop_on_failure: false,
        constraint_overrides: std::collections::HashMap::new(),
        reflection_mode: true,
        provider_override: None,
        model_override: None,
        model_overrides: std::collections::HashMap::new(),
        stage_index: None,
        max_sessions: Some(3),
        auto_run_generated: false,
        approval_gate: false,
        blocking_approval: false,
        confidence_threshold: 0.85,
        max_context_tokens: 100_000,
        enforce_token_budget: false,
        cross_workflow_learning: true,
        verification_history: std::collections::HashMap::new(),
        routing_context: Default::default(),
        project_path: crate::mcp::shared::current_project_path(),
        acceptance_criteria: None,
        multi_agent_mode: false,
        strict_cwd: false,
        tool_tags: Vec::new(),
        use_worktree: false,
        worktree_path: None,
        worktree_branch: None,
        workflow_architecture: None,
        agentic_verification_config: None,
        multi_agent_pipeline_config: None,
        rollback_policy: crate::unified_workflow_executor::RollbackPolicy::None,
        escalation_policy: crate::unified_workflow_executor::blame::EscalationPolicy::default(),
        iteration_diffs: Vec::new(),
        active_canary: None,
        is_canary_run: false,
        phase_timeout_ms: None,
        max_fix_attempts: 3,
        max_ci_auto_resumes: 10,
        ci_failure_context: None,
        htn_config: crate::planning_bridge::HtnConfig::default(),
    }
}

/// Build setup steps that load source run data via curl commands.
pub fn build_setup_steps(
    source_task_run_id: &str,
    source_workflow_name: &str,
    app_state: &crate::commands::AppState,
) -> Vec<ExecutionStepConfig> {
    let base_url = crate::mcp::types::get_self_base_url(app_state);

    vec![
        // Step 1: Load findings from source run
        build_api_step(
            "Load source findings",
            "GET",
            &format!("{}/findings/task/{}", base_url, source_task_run_id),
            None,
            Some("source_findings"),
            true,
        ),
        // Step 2: Load knowledge from source run
        build_api_step(
            "Load source knowledge",
            "GET",
            &format!("{}/task-runs/{}/knowledge", base_url, source_task_run_id),
            None,
            Some("source_knowledge"),
            true,
        ),
        // Step 3: Load AI conversation output (critical for holistic analysis)
        build_api_step(
            "Load AI output",
            "GET",
            &format!(
                "{}/task-runs/{}/output?tail_chars=30000",
                base_url, source_task_run_id
            ),
            None,
            Some("source_ai_output"),
            true,
        ),
        build_api_step(
            "Load pipeline events",
            "GET",
            &format!(
                "{}/graph/pipeline-events?task_run_id={}",
                base_url, source_task_run_id
            ),
            None,
            Some("source_pipeline_events"),
            true,
        ),
        // Step 4: Load workflow execution state
        build_api_step(
            "Load workflow state",
            "GET",
            &format!(
                "{}/task-runs/{}/workflow-state",
                base_url, source_task_run_id
            ),
            None,
            Some("source_workflow_state"),
            true,
        ),
        // Step 5: Load previous reflection fixes for effectiveness evaluation
        // Filter to status=applied to exclude superseded duplicates (reduces context from ~40 to ~15 entries)
        build_api_step(
            "Load previous fixes",
            "GET",
            &format!(
                "{}/reflection-fixes?workflow_name={}&status=applied",
                base_url,
                urlencoding::encode(source_workflow_name)
            ),
            None,
            Some("previous_fixes"),
            true,
        ),
        // Step 6: Load structured already-tried summary (all fixes, grouped by type with DO NOT retry warnings)
        build_api_step(
            "Load already-tried summary",
            "GET",
            &format!(
                "{}/reflection/already-tried-summary?workflow_name={}",
                base_url,
                urlencoding::encode(source_workflow_name)
            ),
            None,
            Some("already_tried_summary"),
            true,
        ),
    ]
}

/// Build verification steps for the reflection workflow.
pub fn build_verification_steps(
    source_task_run_id: &str,
    app_state: &crate::commands::AppState,
) -> Vec<ExecutionStepConfig> {
    let base_url = crate::mcp::types::get_self_base_url(app_state);

    vec![
        // Step 1: Verify reflection fixes were recorded via API
        {
            let mut step = ExecutionStepConfig {
                step_type: "command".to_string(),
                command_mode: Some("check".to_string()),
                name: Some("Verify reflection data accessible".to_string()),
                check_type: Some("http_status".to_string()),
                check_url: Some(format!(
                    "{}/task-runs/{}/knowledge",
                    base_url, source_task_run_id
                )),
                expected_status: Some(200),
                ..Default::default()
            };
            step.phase = Some("verification".to_string());
            step
        },
        // Step 2: Lightweight prompt check (no UI Bridge dependency)
        build_verification_prompt_step(
            "Verify reflection safety",
            r#"Verify that the reflection analysis is complete and no destructive changes were applied.
DO NOT use any UI Bridge SDK tools — they are not available in reflection mode.
Base your verification ONLY on the data already loaded in runtime variables and your own output.

Check:
1. All significant findings from the source run were analyzed
2. No files were deleted or overwritten without recording the change
3. All applied fixes have been recorded via [REFLECTION_FIX:...] markers
4. Fix descriptions are clear and actionable

If any issues are found, report them. Otherwise, confirm the reflection is safe."#,
        ),
    ]
}

/// Build completion steps for the reflection workflow.
pub fn build_completion_steps(
    source_workflow_name: &str,
    source_task_run_id: &str,
    app_state: &crate::commands::AppState,
) -> Vec<ExecutionStepConfig> {
    let base_url = crate::mcp::types::get_self_base_url(app_state);

    vec![
        // Step 1: Batch evaluate previous fixes (automation step)
        build_api_step(
            "Evaluate fix effectiveness",
            "POST",
            &format!(
                "{}/reflection/evaluate?workflow_name={}",
                base_url,
                urlencoding::encode(source_workflow_name)
            ),
            None,
            Some("evaluation_results"),
            false,
        ),
        // Step 2: Build automated causal links from this run's data
        build_api_step(
            "Build causal links",
            "POST",
            &format!(
                "{}/reflection/build-causal-links?task_run_id={}&workflow_name={}",
                base_url,
                source_task_run_id,
                urlencoding::encode(source_workflow_name)
            ),
            None,
            Some("causal_link_results"),
            false,
        ),
        // Step 3: Rebuild architecture model
        build_api_step(
            "Rebuild architecture model",
            "POST",
            &format!(
                "{}/reflection/architecture/rebuild?workflow_name={}",
                base_url,
                urlencoding::encode(source_workflow_name)
            ),
            None,
            Some("architecture_rebuild_results"),
            false,
        ),
        // Step 4: AI summary and pattern recording (prompt step)
        {
            let mut step = build_prompt_step(
                "Record patterns and summarize",
                r#"Complete the reflection by:

1. Record any recurring patterns as knowledge entries using the standard [KNOWLEDGE:recurring_pattern] markers
2. The batch effectiveness evaluation has already been run. Results: {{evaluation_results}}
3. Causal links built: {{causal_link_results}}
4. Architecture model rebuilt: {{architecture_rebuild_results}}
5. Generate a brief summary of:
   - Issues identified (count by category)
   - Fixes applied (count by type)
   - Previous fix effectiveness results
   - Causal relationships identified
   - Architecture model status (components & relationships)
   - Recommendations for the next run"#,
            );
            step.phase = Some("completion".to_string());
            step
        },
    ]
}

/// Build the agentic phase prompt for the reflection AI.
///
/// Dispatches to a generation-specific or execution-specific prompt based on
/// whether the source workflow was a generation meta-workflow ("AI Generate: ...").
fn build_agentic_prompt(source_workflow_name: &str) -> String {
    let is_generation = source_workflow_name.starts_with("AI Generate:");

    let preamble = format!(
        r#"You are a reflection agent analyzing the completed {} for "{}".

{}

## Tool Access

You have full tool access (file read/write, bash, grep, etc.). Your primary job is to analyze
the loaded data and produce REFLECTION_FIX markers, but you may also explore the codebase
when you need to investigate root causes, verify assumptions, or apply fixes directly."#,
        if is_generation {
            "workflow generation run"
        } else {
            "workflow run"
        },
        source_workflow_name,
        if is_generation {
            "Your goal is to identify systemic issues in the workflow generation pipeline and apply fixes \
             that will improve the quality of future generated workflows."
        } else {
            "Your goal is to identify systemic issues and apply fixes that will improve subsequent runs."
        },
    );

    let data_section = r#"
## Data Available

The setup phase loaded the following data into runtime variables:
- `{{source_findings}}` — Categorized findings with signature hashes
- `{{source_knowledge}}` — Observations, solutions, root causes recorded during execution
- `{{source_ai_output}}` — The complete AI conversation output (CRITICAL — read this end-to-end)
- `{{source_workflow_state}}` — Workflow execution state (phases, iterations, timing)
- `{{previous_fixes}}` — Previous reflection fixes for this workflow (for effectiveness comparison)
- `{{already_tried_summary}}` — Structured summary of ALL prior fixes grouped by type, with effectiveness labels and DO NOT retry warnings
- `{{referenced_files}}` — File paths referenced in findings and AI output (validated as existing)
- `{{referenced_file_contents}}` — Pre-loaded contents of referenced files (read these instead of using tools)
- `{{project_structure}}` — Condensed directory tree of the project

## CRITICAL: Already-Tried Context

Review the already-tried summary (`{{already_tried_summary}}`) BEFORE proposing any fixes.
Do NOT re-attempt approaches marked as FAILED or REGRESSION. If a fix type has low effectiveness
across prior attempts, try a fundamentally different strategy rather than variations of the same approach.

## Working Directory

The project root is: {{project_root}}
All relative file paths in findings are relative to this directory.

## Efficiency Guidelines

You have been given pre-loaded data and file contents. Follow these rules:

1. **Start with pre-loaded files.** The referenced file contents below contain the source files mentioned in findings. Read them FIRST before making any tool calls.
2. **Never read the same file twice.** If you already have a file's contents (pre-loaded or via tool), do NOT read it again.
3. **Use the project structure** below to navigate instead of running find, ls, or blind searches.
4. **Use targeted grep** with specific patterns rather than exploratory find/ls commands.

## Pre-loaded File Contents

{{referenced_file_contents}}

## Project Structure

{{project_structure}}"#;

    let marker_section = r#"
## Recording Fixes

Use `[REFLECTION_FIX:...]` markers to record each fix you apply. These are parsed automatically
from your output — no HTTP calls needed.

### Marker Format

```
[REFLECTION_FIX:fix_type:confidence]
Description: What was changed and why
Reasoning: Root cause diagnosis and evidence (optional but recommended)
Alternatives: Other approaches considered and why they were rejected (optional)
Scope: optional scope — 'universal' for patterns that apply across all projects (default: auto-detected)
Applicability: context for when this pattern applies (required when Scope is universal)
File: optional/path/to/file.ext
Old: optional previous value
New: optional new value
Finding: optional-source-finding-id
[/REFLECTION_FIX]
```

**fix_type** must be one of: `knowledge_base_update`, `workflow_step_rewrite`, `selector_fix`, `tool_config_update`, `context_addition`, `instruction_clarification`
(shortcuts: `kb_update`, `step_rewrite`, `selector`, `tool_config`, `context`, `clarification`)

**confidence** must be one of: `high`, `medium`, `low`

**Decision Context:** For each fix, explain WHY in the `Reasoning:` field (root cause diagnosis, evidence from the conversation output, how you identified the issue). Use `Alternatives:` when you considered multiple approaches — document what you weighed and why you chose this approach over others. This structured reasoning is surfaced during effectiveness evaluation when a fix later proves ineffective.

### Universal Patterns

When you identify a pattern that is NOT specific to this project's code or structure,
mark it with `Scope: universal`. Examples:
- "Always use data-testid selectors instead of CSS classes for test automation"
- "Wait for network idle before asserting page content"
- "Set explicit timeouts for CI environments vs local"

Do NOT mark as universal:
- Project-specific file paths or directory structures
- Patterns that depend on a specific codebase's conventions

When marking a fix as universal, always include `Applicability:` describing
when this pattern is relevant for other projects."#;

    let analysis_steps = if is_generation {
        build_generation_analysis_steps()
    } else {
        build_execution_analysis_steps()
    };

    let causal_section = r#"
## Causal Chain Analysis

In addition to recording fixes, identify causal relationships between events.
Use `[CAUSAL_CHAIN:...]` markers to record cause→effect links you observe.

### Marker Format

```
[CAUSAL_CHAIN:relationship]
Cause: event_type:reference
Effect: event_type:reference
Description: What caused what and why
[/CAUSAL_CHAIN]
```

**relationship** must be one of: `caused`, `triggered`, `resolved`, `prevented`

**event_type** must be one of: `code_change`, `finding_detected`, `error_occurred`, `fix_applied`, `verification_passed`, `verification_failed`

### What to look for
- Code changes that caused test failures or errors
- Errors that triggered specific findings
- Fixes that resolved specific errors
- Changes that prevented previously-recurring issues

### Example
```
[CAUSAL_CHAIN:caused]
Cause: code_change:src/api/routes.ts
Effect: finding_detected:API endpoint returns 404
Description: Route handler was moved to a new file but the import path wasn't updated
[/CAUSAL_CHAIN]
```

Focus on the most significant causal relationships (max 5). Don't record trivial ones."#;

    let evaluation_section = r#"
### Step 5: Evaluate Previous Fixes
Compare the source run's findings against previously applied fixes:
- If a fix's source finding signature does NOT appear in this run → fix was effective
- If the signature DOES appear → fix was ineffective
- If new findings appeared that weren't present before → possible regression

Note: Batch effectiveness evaluation runs automatically in the completion phase.
Focus your analysis on qualitative observations about which fixes helped and which didn't,
and record any new fixes needed using `[REFLECTION_FIX:...]` markers."#;

    format!(
        "{}\n{}\n{}\n{}\n{}\n{}\n{}",
        preamble,
        data_section,
        REFLECTION_GUARDRAILS,
        marker_section,
        causal_section,
        analysis_steps,
        evaluation_section
    )
}

/// Analysis steps for reflecting on a workflow execution run.
///
/// Focuses on runtime issues: tool failures, selector problems, missing context,
/// wasted iterations, and decision quality during execution.
fn build_execution_analysis_steps() -> String {
    r#"
### Example

```
[REFLECTION_FIX:context_addition:high]
Description: Added missing workspace root path to the setup context so the AI knows where project files are located
Reasoning: The AI spent 3 iterations searching for project files because it didn't know the workspace root. The conversation output shows repeated 'find' commands across different directories before eventually locating the project at /home/user/project.
Alternatives: Considered adding a file listing step instead, but providing the root path is simpler and lets the AI explore as needed rather than pre-loading a potentially stale file list.
File: workflows/my-workflow.json
Old: No workspace context provided
New: Added workspace_root variable to setup phase
[/REFLECTION_FIX]
```

### API Fallback

If you need to make HTTP calls directly (e.g., to evaluate previous fix effectiveness),
use the `api_request` MCP tool to call the runner API at {{runner_api_url}}.

## Analysis Steps

### Step 1: Holistic Output Analysis (PRIMARY)
Read the complete AI conversation output end-to-end and identify behavioral patterns NOT captured by inline findings:
- **Repeated failed attempts** (e.g., "tried 4 selectors before finding the right one")
- **Goal misunderstanding** (e.g., "spent 2 iterations on wrong approach before correcting")
- **Tool usage struggles** (e.g., "UI Bridge calls consistently failed with timeout")
- **Missing context indicators** (e.g., "AI asked 'where is X?' or searched extensively")
- **Wasted iterations** (e.g., "iteration 3 repeated work already done in iteration 1")
- **Decision quality** (e.g., "AI chose approach A but approach B would have been better")

### Step 2: Cross-reference with Structured Data
- Compare holistic observations against the localized findings and knowledge
- Identify gaps — issues the AI struggled with but didn't emit a [FINDING:...] for
- Note verification feedback patterns — what checks kept failing and why

### Step 3: Categorize All Issues
- `tool_failure` — tool/API/selector problems
- `context_gap` — AI lacked information it should have had
- `instruction_ambiguity` — workflow steps were unclear or misleading
- `recurring_pattern` — same issue seen across iterations or across runs

### Step 4: Apply Fixes and Record
For each actionable issue, apply the fix and record it using a `[REFLECTION_FIX:...]` marker.
The source and reflection task run IDs are filled in automatically — just provide the fix details.

Example:
```
[REFLECTION_FIX:selector_fix:high]
Description: Updated login button selector from #btn-login to button[data-testid="login"]
Reasoning: The old #btn-login ID was removed in a recent frontend refactor (commit abc1234). The conversation output shows the AI tried this selector 3 times with timeouts before falling back to a text-based search.
Alternatives: Considered using button.login-btn class selector, but data-testid attributes are explicitly maintained for testing and are more stable across CSS changes.
Scope: universal
Applicability: Web apps with test automation — prefer data-testid attributes over CSS selectors
File: workflows/login-flow.json
Old: #btn-login
New: button[data-testid="login"]
Finding: finding-abc123
[/REFLECTION_FIX]
```

**Confidence guidelines:**
- `high` — Clear root cause, straightforward fix, apply automatically
- `medium` — Likely fix but needs validation, apply and monitor
- `low` — Speculative, log but don't auto-apply

### Auto-Applied Fix Types
The following fix types are automatically applied by the system:
- `knowledge_base_update` at high/medium confidence → Creates a recurring_pattern knowledge entry
- `context_addition` at high confidence → Creates a context knowledge entry
- `instruction_clarification` at high confidence → Creates a new generation rule for the relevant prompt agent (schema_context, hardener, or verification). Use this for insights about how generation prompts should be improved.
- `workflow_step_rewrite` at high confidence → Creates a new generation rule for the relevant prompt agent. Use this for patterns about how specific step types should be generated differently.

All other fix types are recorded for manual review. Use these auto-applied types when you have
clear, actionable insights that should persist across runs. Avoid low-confidence auto-applied types
as they will only be recorded without creating knowledge entries.

### Deduplication
Fixes are deduplicated by content hash (fix_type + description + old_value + new_value).
If an identical fix already exists with status 'applied', the duplicate will be skipped.
Only emit fixes for genuinely new insights — do NOT re-emit fixes from previous reflection runs."#
        .to_string()
}

/// Analysis steps for reflecting on a workflow generation run.
///
/// Focuses on generation quality: prompt interpretation, step type selection,
/// builder/verifier/fixer agent quality, and structural issues in the output workflow.
fn build_generation_analysis_steps() -> String {
    r#"
### Example

```
[REFLECTION_FIX:instruction_clarification:high]
Description: Builder agent generates command steps instead of prompt steps for AI-driven analysis tasks
Reasoning: In 3 of 5 generated workflows, the builder used shell_command steps for tasks requiring AI reasoning (e.g., "analyze code quality"). The conversation shows the verifier flagged this twice but the fixer only corrected one instance, suggesting the builder prompt lacks clear guidance.
Alternatives: Considered adding a post-generation validation rule to auto-convert mistyped steps, but fixing the builder prompt at the source is more efficient and prevents the issue entirely.
Old: No guidance on prompt vs command step selection
New: Added rule: use prompt steps when the task requires AI reasoning/analysis; use command steps only for deterministic shell operations
[/REFLECTION_FIX]
```

### API Fallback

If you need to make HTTP calls directly (e.g., to evaluate previous fix effectiveness),
use the `api_request` MCP tool to call the runner API at {{runner_api_url}}.

## Analysis Steps

The generation pipeline uses a 3-agent architecture: **Builder** (creates the initial workflow JSON), **Verifier** (reviews for correctness), and **Fixer** (corrects issues found by the verifier). Analyze each agent's performance.

### Step 1: Builder Agent Analysis (PRIMARY)
Read the AI conversation output end-to-end and evaluate the builder's decisions:
- **Prompt interpretation** — Did the builder correctly understand the user's intent from the description? Did it miss key requirements or add unnecessary steps?
- **Step type selection** — Were the right step types chosen (command vs prompt vs check)? Were there cases where a different step type would have been more appropriate?
- **Step configuration quality** — Were steps well-configured with appropriate commands, selectors, timeouts, and expected values? Were there obvious misconfigurations?
- **Workflow structure** — Is the phase organization logical (setup → verification → agentic → completion)? Are steps in the right phases?
- **Missing steps** — Were important steps omitted that the workflow clearly needs?
- **Unnecessary steps** — Were steps included that add no value or are redundant?

### Step 2: Verifier Agent Analysis
Evaluate the quality of the verifier's review:
- **False positives** — Did the verifier flag issues that weren't actually problems, wasting fixer iterations?
- **False negatives** — Did real issues slip through that the verifier should have caught?
- **Feedback quality** — Was the verifier's feedback specific and actionable, or vague and unhelpful?
- **Verification coverage** — Did the verifier check all important aspects (step types, configuration, phase assignments, naming)?

### Step 3: Fixer Agent Analysis
Evaluate the fixer's corrections:
- **Fix quality** — Did fixes actually improve the workflow, or did they introduce new issues?
- **Over-correction** — Did the fixer make unnecessary changes beyond what the verifier flagged?
- **Under-correction** — Did the fixer fail to address issues the verifier identified?
- **Iteration efficiency** — How many fix iterations were needed? Could the workflow have converged faster?

### Step 4: Categorize Issues and Apply Fixes
For each identified issue, determine the best fix type:

- `instruction_clarification` (HIGH PRIORITY for generation) — Improvements to builder/verifier/fixer prompts. These become generation rules that directly improve future workflow generation.
- `workflow_step_rewrite` (HIGH PRIORITY for generation) — Patterns about how specific step types should be generated. These become step-type-specific generation rules.
- `knowledge_base_update` — Recurring generation anti-patterns that should be remembered across runs.
- `context_addition` — Missing context that the generation agents needed (e.g., available step types, configuration schemas, platform capabilities).

Record each fix using a `[REFLECTION_FIX:...]` marker. The source and reflection task run IDs are filled in automatically.

**Confidence guidelines:**
- `high` — Clear pattern seen in the output, straightforward prompt improvement
- `medium` — Likely improvement but based on a single observation
- `low` — Speculative, may need more data points

### Auto-Applied Fix Types
The following fix types are automatically applied by the system:
- `knowledge_base_update` at high/medium confidence → Creates a recurring_pattern knowledge entry
- `context_addition` at high confidence → Creates a context knowledge entry
- `instruction_clarification` at high confidence → Creates a new generation rule for the relevant prompt agent (schema_context, hardener, or verification). **This is the most valuable fix type for generation reflection** — it directly improves the prompts used by the builder, verifier, and fixer agents.
- `workflow_step_rewrite` at high confidence → Creates a new generation rule for the relevant prompt agent. Use this for patterns about how specific step types should be generated differently.

All other fix types are recorded for manual review.

### Deduplication
Fixes are deduplicated by content hash (fix_type + description + old_value + new_value).
If an identical fix already exists with status 'applied', the duplicate will be skipped.
Only emit fixes for genuinely new insights — do NOT re-emit fixes from previous reflection runs."#
        .to_string()
}

// =============================================================================
// Project Reflection Workflow
// =============================================================================
// Learns about the user's project/codebase — environment, architecture, test
// patterns, recurring issues. Runs in both dev and production modes.

/// Build the LoopConfig for a project reflection workflow.
pub fn build_project_reflection_config(
    execution_id: &str,
    workflow_name: &str,
    source_workflow_name: &str,
    project_path: Option<String>,
) -> LoopConfig {
    LoopConfig {
        max_iterations: 3, // Increased from 2: AI verification step needs room to retry
        base_prompt: build_project_agentic_prompt(source_workflow_name),
        workflow_name: workflow_name.to_string(),
        workflow_id: format!("project-reflection-{}", execution_id),
        execution_id: execution_id.to_string(),
        targeted_error_ids: Vec::new(),
        starting_iteration: 0,
        run_agentic_first: true,
        artifact_dir: None,
        is_dev_mode: false, // CRITICAL: Prevents cascade reflection
        enable_sweep: false,
        max_sweep_iterations: 5,
        stages: Vec::new(),
        stop_on_failure: false,
        constraint_overrides: std::collections::HashMap::new(),
        reflection_mode: true,
        provider_override: None,
        model_override: None,
        model_overrides: std::collections::HashMap::new(),
        stage_index: None,
        max_sessions: Some(2),
        auto_run_generated: false,
        approval_gate: false,
        blocking_approval: false,
        confidence_threshold: 0.85,
        max_context_tokens: 80_000,
        enforce_token_budget: false,
        cross_workflow_learning: false, // Not needed — project reflection IS the cross-project learner
        verification_history: std::collections::HashMap::new(),
        routing_context: Default::default(),
        project_path,
        multi_agent_mode: false,
        strict_cwd: false,
        tool_tags: Vec::new(),
        use_worktree: false,
        worktree_path: None,
        worktree_branch: None,
        workflow_architecture: None,
        agentic_verification_config: None,
        multi_agent_pipeline_config: None,
        acceptance_criteria: None,
        rollback_policy: crate::unified_workflow_executor::RollbackPolicy::None,
        escalation_policy: crate::unified_workflow_executor::blame::EscalationPolicy::default(),
        iteration_diffs: Vec::new(),
        active_canary: None,
        is_canary_run: false,
        phase_timeout_ms: None,
        max_fix_attempts: 3,
        max_ci_auto_resumes: 10,
        ci_failure_context: None,
        htn_config: crate::planning_bridge::HtnConfig::default(),
    }
}

/// Build setup steps for project reflection (loads source run data).
pub fn build_project_setup_steps(
    source_task_run_id: &str,
    source_workflow_name: &str,
    app_state: &crate::commands::AppState,
) -> Vec<ExecutionStepConfig> {
    let base_url = crate::mcp::types::get_self_base_url(app_state);

    vec![
        // Step 1: Load findings from source run
        build_api_step(
            "Load source findings",
            "GET",
            &format!("{}/findings/task/{}", base_url, source_task_run_id),
            None,
            Some("source_findings"),
            true,
        ),
        // Step 2: Load knowledge from source run
        build_api_step(
            "Load source knowledge",
            "GET",
            &format!("{}/task-runs/{}/knowledge", base_url, source_task_run_id),
            None,
            Some("source_knowledge"),
            true,
        ),
        // Step 3: Load AI conversation output
        build_api_step(
            "Load AI output",
            "GET",
            &format!(
                "{}/task-runs/{}/output?tail_chars=30000",
                base_url, source_task_run_id
            ),
            None,
            Some("source_ai_output"),
            true,
        ),
        build_api_step(
            "Load pipeline events",
            "GET",
            &format!(
                "{}/graph/pipeline-events?task_run_id={}",
                base_url, source_task_run_id
            ),
            None,
            Some("source_pipeline_events"),
            true,
        ),
        // Step 4: Load workflow execution state
        build_api_step(
            "Load workflow state",
            "GET",
            &format!(
                "{}/task-runs/{}/workflow-state",
                base_url, source_task_run_id
            ),
            None,
            Some("source_workflow_state"),
            true,
        ),
        // Step 5: Load previous project-scoped fixes
        build_api_step(
            "Load previous project fixes",
            "GET",
            &format!(
                "{}/reflection-fixes?workflow_name={}&status=applied",
                base_url,
                urlencoding::encode(source_workflow_name)
            ),
            None,
            Some("previous_fixes"),
            true,
        ),
        // Step 6: Load structured already-tried summary
        build_api_step(
            "Load already-tried summary",
            "GET",
            &format!(
                "{}/reflection/already-tried-summary?workflow_name={}",
                base_url,
                urlencoding::encode(source_workflow_name)
            ),
            None,
            Some("already_tried_summary"),
            true,
        ),
    ]
}

/// Build verification steps for project reflection (simplified — safety check only).
pub fn build_project_verification_steps(
    source_task_run_id: &str,
    app_state: &crate::commands::AppState,
) -> Vec<ExecutionStepConfig> {
    let base_url = crate::mcp::types::get_self_base_url(app_state);

    vec![
        // Step 1: HTTP health check (non-blocking — AI step below provides fallback)
        {
            let mut step = ExecutionStepConfig {
                step_type: "command".to_string(),
                command_mode: Some("check".to_string()),
                name: Some("Verify source data accessible".to_string()),
                check_type: Some("http_status".to_string()),
                check_url: Some(format!(
                    "{}/task-runs/{}/knowledge",
                    base_url, source_task_run_id
                )),
                expected_status: Some(200),
                ..Default::default()
            };
            step.phase = Some("verification".to_string());
            step
        },
        // Step 2: AI-driven verification (matches pattern from build_verification_steps)
        build_verification_prompt_step(
            "Verify project reflection safety",
            r#"Verify that the project reflection analysis is complete and safe.
DO NOT use any UI Bridge SDK tools — they are not available in reflection mode.
Base your verification ONLY on the data already loaded in runtime variables and your own output.

Check:
1. The AI analyzed the source output and identified relevant project-level patterns
2. Project knowledge was recorded via [REFLECTION_FIX:...] markers OR the AI confirmed no new project knowledge was found
3. No destructive changes were made (no files deleted or overwritten without recording)

If any issues are found, report them. Otherwise, confirm the project reflection is safe."#,
        ),
    ]
}

/// Build completion steps for project reflection.
pub fn build_project_completion_steps(
    source_workflow_name: &str,
    app_state: &crate::commands::AppState,
) -> Vec<ExecutionStepConfig> {
    let base_url = crate::mcp::types::get_self_base_url(app_state);

    vec![
        // Step 1: Batch evaluate previous fixes
        build_api_step(
            "Evaluate fix effectiveness",
            "POST",
            &format!(
                "{}/reflection/evaluate?workflow_name={}",
                base_url,
                urlencoding::encode(source_workflow_name)
            ),
            None,
            Some("evaluation_results"),
            false,
        ),
        // Step 2: AI summary
        {
            let mut step = build_prompt_step(
                "Summarize project learnings",
                r#"Summarize what was learned about this project:

1. List the project knowledge entries you recorded (by category)
2. Batch effectiveness results: {{evaluation_results}}
3. Brief assessment: Are there important aspects of the project still not documented?

Keep the summary concise — this is for internal tracking, not user display."#,
            );
            step.phase = Some("completion".to_string());
            step
        },
    ]
}

/// Build the agentic prompt for project reflection.
///
/// Unlike workflow reflection which focuses on fixing workflow mechanics,
/// project reflection focuses on learning about the user's project/codebase.
fn build_project_agentic_prompt(source_workflow_name: &str) -> String {
    let prompt = format!(
        r#"You are a project reflection agent analyzing the completed workflow run for "{}".

Your goal is NOT to fix workflows — instead, you are learning about the **user's project**.
Extract lasting knowledge about the project's environment, architecture, test patterns,
and recurring issues. This knowledge will be injected into future workflows targeting
the same project directory.

## Tool Access

You have full tool access (file read/write, bash, grep, etc.). Use it to explore the
project directory when the AI output suggests the workflow struggled with something
project-specific (e.g., "couldn't find the test runner", "wrong working directory").

## Data Available

The setup phase loaded the following data into runtime variables:
- `{{{{source_findings}}}}` — Categorized findings from the workflow run
- `{{{{source_knowledge}}}}` — Knowledge recorded during execution
- `{{{{source_ai_output}}}}` — The complete AI conversation output (CRITICAL — read this end-to-end)
- `{{{{source_workflow_state}}}}` — Workflow execution state
- `{{{{previous_fixes}}}}` — Previous project reflection knowledge (for deduplication)
- `{{{{already_tried_summary}}}}` — Structured summary of ALL prior fixes grouped by type, with effectiveness labels and DO NOT retry warnings
- `{{{{referenced_files}}}}` — File paths referenced in findings and AI output (validated as existing)
- `{{{{referenced_file_contents}}}}` — Pre-loaded contents of referenced files (read these instead of using tools)
- `{{{{project_structure}}}}` — Condensed directory tree of the project

## CRITICAL: Already-Tried Context

Review the already-tried summary (`{{{{already_tried_summary}}}}`) BEFORE proposing any fixes.
Do NOT re-attempt approaches marked as FAILED or REGRESSION. If a fix type has low effectiveness
across prior attempts, try a fundamentally different strategy rather than variations of the same approach.

## Working Directory

The project root is: {{{{project_root}}}}
All relative file paths in findings are relative to this directory.

## Efficiency Guidelines

You have been given pre-loaded data and file contents. Follow these rules:

1. **Start with pre-loaded files.** The referenced file contents below contain the source files mentioned in findings. Read them FIRST before making any tool calls.
2. **Never read the same file twice.** If you already have a file's contents (pre-loaded or via tool), do NOT read it again.
3. **Use the project structure** below to navigate instead of running find, ls, or blind searches.
4. **Use targeted grep** with specific patterns rather than exploratory find/ls commands.

## Pre-loaded File Contents

{{{{referenced_file_contents}}}}

## Project Structure

{{{{project_structure}}}}

## Recording Project Knowledge

Use `[REFLECTION_FIX:...]` markers with the **project-scoped fix types**:

### Fix Types

| Type | Shortcut | Use for |
|------|----------|---------|
| `project_environment` | `proj_env` | Dependencies, env vars, runtime versions, required services |
| `project_architecture` | `proj_arch` | Where tests live, routes location, framework patterns, build system |
| `project_test_pattern` | `proj_test` | Test runner, fixture patterns, setup/teardown, docker requirements |
| `project_recurring_issue` | `proj_issue` | Flaky areas, slow operations, known quirks, common failure modes |

### Marker Format

```
[REFLECTION_FIX:project_environment:high]
Description: Project uses pnpm (not npm). Package install commands must use `pnpm install`.
[/REFLECTION_FIX]
```

**confidence** must be: `high`, `medium`, or `low`

## Analysis Steps

### Step 1: Read the AI Conversation Output (PRIMARY)
Read `{{{{source_ai_output}}}}` end-to-end. Look for:

1. **Environment requirements** the AI discovered or struggled with:
   - Did the AI fail because a dependency wasn't installed?
   - Did it use the wrong package manager, runtime version, or env var?
   - Were specific services (database, redis, etc.) required?

2. **Codebase structure** the AI had to figure out:
   - Did the AI search extensively for files? Where did it find them?
   - What framework patterns did it identify (e.g., "tests co-located with source")?
   - What build system is used? How are things organized?

3. **Test infrastructure** patterns:
   - What test runner is used? What fixture/setup patterns exist?
   - Are integration tests separate from unit tests? Do they need docker/services?
   - Are there conftest.py files, jest.config, vitest.config, etc.?

4. **Recurring friction points**:
   - Did the same type of error happen multiple times?
   - Were there flaky tests or timing-sensitive operations?
   - Were there known workarounds the AI had to discover?

### Step 2: Cross-reference with Findings
Check `{{{{source_findings}}}}` and `{{{{source_knowledge}}}}` for project-specific patterns
that the AI documented during execution.

### Step 3: Explore the Project (if needed)
If the AI output suggests the workflow struggled with project-specific issues,
use file tools to explore the project directory and verify your observations.

### Step 4: Record Project Knowledge
For each observation, emit a `[REFLECTION_FIX:...]` marker. Only emit genuinely
useful, project-specific knowledge. Do NOT emit:
- Workflow mechanics fixes (those belong in workflow reflection)
- Generic programming advice
- Things already in the previous fixes list

**Confidence guidelines:**
- `high` — Clearly observed in the AI output, verified by exploration
- `medium` — Inferred from the AI output, plausible but not directly confirmed
- `low` — Speculative, based on limited evidence

### Deduplication
Fixes are deduplicated by content hash. Check `{{{{previous_fixes}}}}` before emitting.
Only emit genuinely new insights — do NOT re-emit existing knowledge."#,
        source_workflow_name
    );
    format!("{}\n{}", prompt, REFLECTION_GUARDRAILS)
}

/// Helper: Build a command step that makes an HTTP request via curl.
///
/// Constructs a `command` step with a curl shell command.
/// The `output_variable` is extracted from the step output by the runtime.
fn build_api_step(
    name: &str,
    method: &str,
    url: &str,
    body: Option<&str>,
    output_variable: Option<&str>,
    is_setup: bool,
) -> ExecutionStepConfig {
    // Build curl command
    let curl_cmd = if let Some(body_str) = body {
        format!(
            "curl -sf -X {} -H \"Content-Type: application/json\" -d '{}' '{}'",
            method, body_str, url
        )
    } else {
        format!("curl -sf -X {} '{}'", method, url)
    };

    // If we need to capture output into a variable, use extract
    let extract = output_variable.map(|var| {
        let mut map = HashMap::new();
        map.insert(var.to_string(), "$".to_string());
        map
    });

    let mut step = ExecutionStepConfig {
        step_type: "command".to_string(),
        command_mode: Some("shell".to_string()),
        name: Some(name.to_string()),
        shell_command: Some(curl_cmd),
        extract,
        ..Default::default()
    };
    if is_setup {
        step.phase = Some("setup".to_string());
        step.run_on_subsequent_iterations = Some(false);
    } else {
        step.phase = Some("completion".to_string());
    }
    step
}

// =============================================================================
// UI Bridge Reflection Workflow
// =============================================================================
// Analyzes how the UI Bridge was used (or not used) in a workflow run,
// identifies improvements, and automatically implements them. Runs in dev mode only.

/// Build the LoopConfig for a UI Bridge reflection workflow.
pub fn build_ui_bridge_reflection_config(
    execution_id: &str,
    workflow_name: &str,
    source_workflow_name: &str,
) -> LoopConfig {
    LoopConfig {
        max_iterations: 5,
        base_prompt: build_ui_bridge_agentic_prompt(source_workflow_name),
        workflow_name: workflow_name.to_string(),
        workflow_id: format!("ui-bridge-reflection-{}", execution_id),
        execution_id: execution_id.to_string(),
        targeted_error_ids: Vec::new(),
        starting_iteration: 0,
        run_agentic_first: true,
        artifact_dir: None,
        is_dev_mode: false, // CRITICAL: Prevents cascade reflection
        enable_sweep: false,
        max_sweep_iterations: 5,
        stages: Vec::new(),
        stop_on_failure: false,
        constraint_overrides: std::collections::HashMap::new(),
        reflection_mode: true,
        provider_override: None,
        model_override: None,
        model_overrides: std::collections::HashMap::new(),
        stage_index: None,
        max_sessions: Some(5),
        auto_run_generated: false,
        approval_gate: false,
        blocking_approval: false,
        confidence_threshold: 0.85,
        max_context_tokens: 500_000,
        enforce_token_budget: false,
        cross_workflow_learning: true,
        verification_history: std::collections::HashMap::new(),
        routing_context: Default::default(),
        multi_agent_mode: false,
        strict_cwd: false,
        tool_tags: Vec::new(),
        use_worktree: false,
        worktree_path: None,
        worktree_branch: None,
        workflow_architecture: None,
        agentic_verification_config: None,
        multi_agent_pipeline_config: None,
        project_path: crate::mcp::shared::current_project_path(),
        acceptance_criteria: None,
        rollback_policy: crate::unified_workflow_executor::RollbackPolicy::None,
        escalation_policy: crate::unified_workflow_executor::blame::EscalationPolicy::default(),
        iteration_diffs: Vec::new(),
        active_canary: None,
        is_canary_run: false,
        phase_timeout_ms: None,
        max_fix_attempts: 3,
        max_ci_auto_resumes: 10,
        ci_failure_context: None,
        htn_config: crate::planning_bridge::HtnConfig::default(),
    }
}

/// Build setup steps for UI Bridge reflection (loads source run data + UI Bridge logs).
pub fn build_ui_bridge_setup_steps(
    source_task_run_id: &str,
    source_workflow_name: &str,
    app_state: &crate::commands::AppState,
) -> Vec<ExecutionStepConfig> {
    let base_url = crate::mcp::types::get_self_base_url(app_state);

    vec![
        // Step 1: Load AI conversation output (primary data source)
        build_api_step(
            "Load AI output",
            "GET",
            &format!(
                "{}/task-runs/{}/output?tail_chars=50000",
                base_url, source_task_run_id
            ),
            None,
            Some("source_ai_output"),
            true,
        ),
        build_api_step(
            "Load pipeline events",
            "GET",
            &format!(
                "{}/graph/pipeline-events?task_run_id={}",
                base_url, source_task_run_id
            ),
            None,
            Some("source_pipeline_events"),
            true,
        ),
        // Step 2: Load workflow execution state (phases, steps, timing)
        build_api_step(
            "Load workflow state",
            "GET",
            &format!(
                "{}/task-runs/{}/workflow-state",
                base_url, source_task_run_id
            ),
            None,
            Some("source_workflow_state"),
            true,
        ),
        // Step 3: Load findings from source run
        build_api_step(
            "Load source findings",
            "GET",
            &format!("{}/findings/task/{}", base_url, source_task_run_id),
            None,
            Some("source_findings"),
            true,
        ),
        // Step 4: Load step checkpoints (includes UI Bridge step results)
        build_api_step(
            "Load step checkpoints",
            "GET",
            &format!("{}/task-runs/{}/checkpoints", base_url, source_task_run_id),
            None,
            Some("source_checkpoints"),
            true,
        ),
        // Step 5: Load previous UI Bridge reflection fixes for deduplication
        build_api_step(
            "Load previous UI Bridge fixes",
            "GET",
            &format!(
                "{}/reflection-fixes?workflow_name={}&status=applied",
                base_url,
                urlencoding::encode(source_workflow_name)
            ),
            None,
            Some("previous_fixes"),
            true,
        ),
        // Step 6: Load already-tried summary
        build_api_step(
            "Load already-tried summary",
            "GET",
            &format!(
                "{}/reflection/already-tried-summary?workflow_name={}",
                base_url,
                urlencoding::encode(source_workflow_name)
            ),
            None,
            Some("already_tried_summary"),
            true,
        ),
    ]
}

/// Build verification steps for UI Bridge reflection.
pub fn build_ui_bridge_verification_steps(
    source_task_run_id: &str,
    app_state: &crate::commands::AppState,
) -> Vec<ExecutionStepConfig> {
    let base_url = crate::mcp::types::get_self_base_url(app_state);

    vec![
        // HTTP health check
        {
            let mut step = ExecutionStepConfig {
                step_type: "command".to_string(),
                command_mode: Some("check".to_string()),
                name: Some("Verify source data accessible".to_string()),
                check_type: Some("http_status".to_string()),
                check_url: Some(format!(
                    "{}/task-runs/{}/knowledge",
                    base_url, source_task_run_id
                )),
                expected_status: Some(200),
                ..Default::default()
            };
            step.phase = Some("verification".to_string());
            step
        },
        // Verify implementations compile/typecheck
        build_verification_prompt_step(
            "Verify UI Bridge improvements",
            r#"Verify that any code changes made to the UI Bridge SDK, runner, or workflow prompts are safe:

1. If TypeScript files were modified in ui-bridge/, run: cd ui-bridge && npm run build
2. If Rust files were modified in qontinui-runner/, run: cd qontinui-runner/src-tauri && cargo check
3. If markdown command files were modified, verify they are syntactically correct
4. Confirm all REFLECTION_FIX markers were emitted for changes made

Report any compilation errors or issues found."#,
        ),
    ]
}

/// Build completion steps for UI Bridge reflection.
///
/// Includes: effectiveness evaluation, implementation workflow generation from the
/// plan produced by the agentic phase, and a summary.
pub fn build_ui_bridge_completion_steps(
    source_workflow_name: &str,
    app_state: &crate::commands::AppState,
) -> Vec<ExecutionStepConfig> {
    let base_url = crate::mcp::types::get_self_base_url(app_state);

    vec![
        // Step 1: Batch evaluate previous fixes (automation)
        build_api_step(
            "Evaluate fix effectiveness",
            "POST",
            &format!(
                "{}/reflection/evaluate?workflow_name={}",
                base_url,
                urlencoding::encode(source_workflow_name)
            ),
            None,
            Some("evaluation_results"),
            false,
        ),
        // Step 2: Load this reflection's own output so the completion phase can
        // find the [UI_BRIDGE_IMPLEMENTATION_PLAN] block produced by the agentic phase.
        // This is needed because completion runs in a new AI session without the
        // agentic phase's conversation context.
        build_api_step(
            "Load reflection output for plan extraction",
            "GET",
            &format!(
                "{}/task-runs/{{{{execution_id}}}}/output?tail_chars=60000",
                base_url
            ),
            None,
            Some("reflection_output"),
            false,
        ),
        // Step 3: Generate implementation workflow from the plan (prompt step)
        {
            let mut step = build_prompt_step(
                "Generate implementation workflow",
                &format!(
                    r#"Generate an implementation workflow from the UI Bridge improvement plan.

## The Reflection Output

The agentic phase's output has been loaded into `{{{{reflection_output}}}}`. Search it for the `[UI_BRIDGE_IMPLEMENTATION_PLAN]...[/UI_BRIDGE_IMPLEMENTATION_PLAN]` block.

## Instructions

1. **Extract the plan**: Find the `[UI_BRIDGE_IMPLEMENTATION_PLAN]...[/UI_BRIDGE_IMPLEMENTATION_PLAN]` block from `{{{{reflection_output}}}}`. If no plan block exists (e.g., no improvements identified), skip this step and report "No implementation plan — no improvements needed."

2. **Generate the workflow**: Call the workflow generator API with the full plan text as the description:

```bash
curl -s -X POST '{base_url}/unified-workflows/generate-async' \
  -H 'Content-Type: application/json' \
  -d '{{
    "description": "<INSERT THE FULL IMPLEMENTATION PLAN TEXT HERE>",
    "category": "ui-bridge-improvement",
    "tags": ["ui-bridge", "reflection", "auto-generated"],
    "generate_specification": true,
    "verification_depth": "thorough",
    "investigation_codebase": true,
    "reflection_mode": true,
    "include_ui_bridge_instructions": true,
    "discover_ui_bridge_specs": true,
    "auto_run": true,
    "inline_context": "This workflow implements UI Bridge improvements identified by the UI Bridge reflection system. The improvements are prioritized P0-P2. Each improvement has acceptance criteria that must be verified. On completion, run /review-plan to review the implementation."
  }}'
```

3. **Record the result**: Save the returned `task_run_id` and `meta_workflow_id` for tracking.

4. **If the generator returns an error**, fall back to recording the plan as a dev note:
   - Write the full plan to `qontinui-dev-notes/ui-bridge-implementation-plan.md`
   - Report that manual implementation is needed

## Output

Report:
- Whether a workflow was generated
- The task_run_id of the generated workflow (if successful)
- The number of improvements in the plan
- Whether auto_run was triggered"#,
                    base_url = base_url,
                ),
            );
            step.phase = Some("completion".to_string());
            step
        },
        // Step 3: AI summary (prompt step)
        {
            let mut step = build_prompt_step(
                "Summarize UI Bridge reflection",
                r#"Summarize the UI Bridge reflection:

1. List improvements analyzed and quick wins implemented directly
2. Batch effectiveness results: {{evaluation_results}}
3. Implementation workflow status: was one generated? What task_run_id?
4. Expected impact on next workflow run's UI Bridge effectiveness
5. Number of improvements in the implementation plan by priority (P0/P1/P2)

Keep the summary concise — this is for internal tracking."#,
            );
            step.phase = Some("completion".to_string());
            step
        },
    ]
}

/// Build the agentic prompt for UI Bridge reflection.
///
/// This prompt instructs the AI to analyze UI Bridge usage patterns in the source
/// workflow, identify improvements, implement quick wins directly, and produce a
/// structured implementation plan for larger improvements that will be used to
/// generate a follow-up implementation workflow.
fn build_ui_bridge_agentic_prompt(source_workflow_name: &str) -> String {
    let prompt = format!(
        r#"You are a UI Bridge reflection agent analyzing the completed workflow run for "{}".

Your goal is to understand how the UI Bridge was used (both deterministic steps and AI-driven interactions) in the workflow, identify what could be improved, and either implement improvements directly or produce a structured implementation plan for a follow-up workflow.

## UI Bridge Statement of Purpose

All improvements must align with the UI Bridge's core purpose and principles. Read this before designing any changes:

**Mission**: UI Bridge makes any React application semantically observable and programmatically controllable by AI agents, automation workflows, and developers — without brittle selectors, external browser drivers, or manual test instrumentation.

**Core Principle**: The application knows itself better than any external tool can. By embedding observability into the React rendering lifecycle, UI Bridge captures semantic meaning, component relationships, and application state that external tools can only approximate.

**Vision**: The ultimate UI Bridge gives an AI everything it needs to see to understand, develop, and debug an application — efficiently. Any capability that helps an AI see more, understand more, or act more precisely is a welcome addition.

**Five Core Capabilities**:
1. **Discovery and Control** — Element registry with semantic IDs, uniform action API with structured feedback
2. **Semantic Page Specs** — Declarative JSON specs that define page purpose, visual design, architecture, and domain logic correctness. Specs can reference any code available to the AI (backend libraries, data models). An AI reads a spec to understand what functionality *should* do, uses UI Bridge to see what it *currently* does, and the gap is the work. This enables autonomous workflows to develop entire applications.
3. **Model-Based State Machine** — Automatic pathfinding to any place in the UI. Say "navigate to dashboard" instead of writing sequential click scripts. Supports multiple active states, multi-target pathfinding, and fingerprint-based state discovery. Reduces complexity from exponential to polynomial.
4. **Observation and Readiness** — Multi-signal idle detection, render logs, console/network/navigation event capture, visual captures, design inspection
5. **AI-Native Bridge** — Fuzzy search, semantic snapshots, natural language assertions, change summarization, pixel comparison, layout/component diffs

**Design Principles**:
1. **Semantic over structural** — Elements identified by purpose and meaning, not CSS paths
2. **Embedded, not bolted on** — SDK lives inside the app via React hooks
3. **AI-native by default** — Concise structured responses, fuzzy matching, error messages that guide the AI
4. **Readiness, not timeouts** — Composable readiness signals, not arbitrary sleeps
5. **Layered abstraction** — Element-level → component-level → workflow-level
6. **Cross-platform uniformity** — Same HTTP API across browser, desktop, mobile
7. **Observable by design** — Every action returns state before/after, errors, timing
8. **Declarative correctness** — Specs define what the app should do; the state machine defines how to reach any part of it

**Architectural Commitments**:
- The element registry is the source of truth
- HTTP is the primary consumer interface; IPC/MCP/clients are adapters
- The SDK must remain lightweight in the host app
- Server adapters are thin wrappers; business logic lives in core SDK
- AI features are additive, not required
- Specs are the contract between intent and implementation
- The state machine abstracts navigation; consumers declare where, not how

## Tool Access

You have full tool access (file read/write, bash, grep, etc.). Use these to implement quick wins directly and to read code when designing larger improvements.

## Data Available

The setup phase loaded the following data into runtime variables:
- `{{{{source_ai_output}}}}` — The complete AI conversation output (CRITICAL — read this end-to-end)
- `{{{{source_workflow_state}}}}` — Workflow execution state (phases, iterations, timing)
- `{{{{source_findings}}}}` — Categorized findings from the workflow run
- `{{{{source_checkpoints}}}}` — Step-by-step execution results (includes UI Bridge step outcomes)
- `{{{{previous_fixes}}}}` — Previous UI Bridge reflection fixes (for deduplication)
- `{{{{already_tried_summary}}}}` — Structured summary of ALL prior fixes with effectiveness labels
- `{{{{referenced_files}}}}` — File paths referenced in findings and AI output (validated as existing)
- `{{{{referenced_file_contents}}}}` — Pre-loaded contents of referenced files (read these instead of using tools)
- `{{{{project_structure}}}}` — Condensed directory tree of the project

## CRITICAL: Already-Tried Context

Review `{{{{already_tried_summary}}}}` BEFORE proposing any fixes.
Do NOT re-attempt approaches marked as FAILED or REGRESSION.

## Working Directory

The project root is: {{{{project_root}}}}
All relative file paths in findings are relative to this directory.

## Efficiency Guidelines

You have been given pre-loaded data and file contents. Follow these rules:

1. **Start with pre-loaded files.** The referenced file contents below contain the source files mentioned in findings. Read them FIRST before making any tool calls.
2. **Never read the same file twice.** If you already have a file's contents (pre-loaded or via tool), do NOT read it again.
3. **Use the project structure** below to navigate instead of running find, ls, or blind searches.
4. **Use targeted grep** with specific patterns rather than exploratory find/ls commands.

## Pre-loaded File Contents

{{{{referenced_file_contents}}}}

## Project Structure

{{{{project_structure}}}}

## Phase 1: Analysis

Read ALL the data carefully, then answer these questions with specific evidence:

### Q1: UI Bridge Usage Map
Create a map of every moment the UI Bridge was used or could have been used:
- **Deterministic steps**: workflow steps with step_type containing "ui_bridge" (click, type, snapshot, discover, etc.)
- **AI-driven usage**: places in the AI conversation where curl/HTTP calls to UI Bridge endpoints were made
- **Missed opportunities**: places where the AI used screenshots, logs, guesswork, or Playwright instead of querying UI Bridge

### Q2: What Failed?
For each UI Bridge failure (deterministic or AI-driven):
- What endpoint was called and what was the error/unexpected response?
- Was the element not discovered, stale, wrong ID, action unsupported?
- Did the AI retry or fall back to another approach?
- Could the UI Bridge SDK have prevented this with better design?

### Q3: Infrastructure Health Check
Before assuming failures are workflow-level issues, check whether the SDK infrastructure itself is working:
- **Web relay health**: `curl http://localhost:3001/api/ui-bridge/health` — is a browser tab connected? Are there SSE listeners?
- **Runner health**: `curl {{runner_api_url}}/ui-bridge/health` — is the app responsive?
- **Element registration**: Do snapshots return elements? If 0 elements despite a connected browser, the issue is in the SDK, not the workflow.
- **Command round-trip**: Are commands sent via SSE/WebSocket getting responses from the browser? Check response times — if snapshot takes >2s, the browser may not be responding (timeout + fallback to empty cache).
- **Component lifecycle**: Are expected components registered? Components only exist when their page is mounted.

If infrastructure issues are found, trace the root cause through the SDK code. The data flow is:
1. External caller → API route handler (Next.js catch-all or Tauri handler)
2. → Server relay-handlers.ts (relay.queueCommand or cached snapshot)
3. → SSE/WebSocket → Browser CommandRelayListener (useCommandRelay.ts)
4. → commandHandlers.ts executeCommand() → reads from global registry
5. → Response sent back via POST /commands

Common root causes to check:
- **Stale React memoization**: `useMemo`/`useCallback` in useUIBridge.ts capture arrays at mount time. If the dependency (context) doesn't change when registry contents change, the memoized value is stale. The global registry (`getGlobalRegistry()`) is the live source of truth.
- **AutoRegisterProvider not scanning**: Check if `enabled` prop is true, if MutationObserver is attached to document.body, and if the debounce timer has fired.
- **Relay command timeout**: SSE timeout is 8s. If the browser doesn't respond in time, relay-handlers falls back to cached (possibly empty) data silently.

### Q4: What Was Missing?
- Did the AI need information the UI Bridge doesn't expose? (React state, relationships, layout)
- Were element IDs unstable or unpredictable?
- Was the snapshot too noisy/large for effective AI parsing?
- Were higher-level composite actions needed?

### Q5: What Would Have Made the Workflow More Effective?
- Fewer round-trips? Better element labeling? Semantic grouping?
- Auto-discovery before snapshots? Better idle detection?
- Richer error messages from failed actions?

## Phase 2: Implementation

Implement improvements directly in this session, starting with the most impactful. You have full tool access and can make changes across all repos.

### Priority 1: SDK Code Fixes
If Phase 1 Q3 (Infrastructure Health Check) identified SDK bugs or architectural issues, fix them first. These are the highest-impact fixes because they unblock everything else. Examples:
- Stale data bugs (memoization, caching, closure captures)
- Missing or broken endpoints
- Data inconsistencies between endpoints (e.g., component list vs detail returning different data)
- Relay communication failures
- Registry lifecycle issues

**How to trace SDK bugs**: Follow the data flow from the API endpoint through relay-handlers.ts, through the SSE/WebSocket relay, to commandHandlers.ts, to the registry. Check each layer for stale data, missing error handling, or broken wiring. The key files are:
- `ui-bridge/packages/ui-bridge/src/server/relay-handlers.ts` — server-side handlers
- `ui-bridge/packages/ui-bridge/src/react/commandHandlers.ts` — browser-side command execution
- `ui-bridge/packages/ui-bridge/src/react/useCommandRelay.ts` — browser relay listener
- `ui-bridge/packages/ui-bridge/src/react/useUIBridge.ts` — React hook (memoized values)
- `ui-bridge/packages/ui-bridge/src/core/registry.ts` — element/component/workflow registry (source of truth)
- `ui-bridge/packages/ui-bridge/src/control/action-executor.ts` — action execution

### Priority 2: Error Message Improvements
- Better error messages from action execution
- Clearer feedback when elements aren't found
- Actionable messages that tell the AI what to do next (e.g., which page to navigate to)

### Priority 3: Prompt/Guidance Improvements
- Update ui-bridge.md, ufix.md, test-ui-bridge.md with better guidance
- Add UI Bridge usage examples to workflow prompts

### Priority 4: SDK Feature Improvements
- Label improvements, noise reduction in snapshots
- Discovery tweaks, new endpoints

After implementing, test compilation:
- TypeScript: `cd ui-bridge/packages/ui-bridge && npx tsc --noEmit && npm run build`
- Rust: `cd qontinui-runner/src-tauri && cargo check`

## Phase 3: Implementation Plan (All Improvements)

For ALL improvements — including architectural changes, multi-file features, new endpoints, and breaking API changes — produce a structured implementation plan. This plan will be used to generate a follow-up implementation workflow with acceptance criteria, verification steps, and agentic fix loops.

**IMPORTANT**: Do NOT hold back improvements because they seem "too large". The statement of purpose above guides architectural decisions. Every improvement that aligns with the purpose should be planned.

Write the implementation plan as a `[UI_BRIDGE_IMPLEMENTATION_PLAN]` block in your output using this exact format:

```
[UI_BRIDGE_IMPLEMENTATION_PLAN]
# UI Bridge Improvement Implementation Plan

## Summary
One paragraph describing the overall improvements needed and their expected impact.

## Improvements

### 1. [Improvement Title]
**Category**: SDK / Runner / Prompt / Architecture / New Feature
**Priority**: P0 / P1 / P2
**Evidence**: [Specific reference to what was observed in the workflow data]
**Description**: [2-3 sentences describing the improvement]
**Acceptance Criteria**:
- [ ] [Testable criterion 1]
- [ ] [Testable criterion 2]
- [ ] [Testable criterion 3]
**Files to Modify**:
- `path/to/file.ts` — [what changes]
- `path/to/file.rs` — [what changes]
**Verification Method**: command / ui_bridge / test / manual
**Verification Command**: [If method is command, the shell command to verify]

### 2. [Next Improvement]
...

## Testing Strategy
How to verify all improvements work together. Include:
- Compilation checks (TypeScript, Rust)
- A workflow to re-run that exercises the UI Bridge
- Specific UI Bridge endpoints to call to verify improvements

## Dependencies
Any ordering constraints between improvements (e.g., "Improvement 3 depends on 1").
[/UI_BRIDGE_IMPLEMENTATION_PLAN]
```

**Guidelines for the plan:**
- Every improvement MUST have testable acceptance criteria
- Acceptance criteria should be verifiable by a workflow (commands, UI Bridge assertions, or tests)
- Include BOTH the quick wins you already implemented AND the larger improvements
- For already-implemented quick wins, note "Status: Implemented" and focus criteria on verification
- Order improvements by priority (P0 first)
- Be specific about files and what changes — the plan will be given to a workflow generator

## Recording Fixes

Use `[REFLECTION_FIX:...]` markers for every improvement (quick wins you implemented directly):

### Fix Types

| Type | Shortcut | Use for |
|------|----------|---------|
| `ui_bridge_sdk_code_fix` | `ub_code_fix` | **SDK bug fixes**: stale data, broken endpoints, memoization bugs, relay issues, registry inconsistencies. Use this for any fix to the SDK source code that corrects incorrect behavior. |
| `ui_bridge_discovery` | `ub_discovery` | Element discovery improvements: labeling, grouping, hierarchy, semantic roles |
| `ui_bridge_snapshot` | `ub_snapshot` | Snapshot format: structure, AI-friendliness, noise reduction |
| `ui_bridge_action` | `ub_action` | Action reliability: feedback, error recovery, new actions |
| `ui_bridge_sdk_feature` | `ub_sdk_feature` | SDK integration: new endpoints, capabilities, configuration |
| `ui_bridge_relay` | `ub_relay` | Relay/transport fixes: SSE, WebSocket, command queueing, timeout handling, caching |
| `ui_bridge_prompt_guidance` | `ub_prompt` | Prompt/workflow guidance: better instructions for AI to use UI Bridge |

### Marker Format

```
[REFLECTION_FIX:ui_bridge_discovery:high]
Description: Added semantic grouping to element discovery so AI can identify form sections
Reasoning: AI output showed 3 attempts to find the right form field among 47 discovered elements
File: ui-bridge/packages/ui-bridge/src/discovery/scanner.ts
Old: // previous code snippet
New: // new code snippet
[/REFLECTION_FIX]
```

**confidence** must be: `high`, `medium`, or `low`

## Deduplication

Check `{{{{previous_fixes}}}}` before emitting. Only emit genuinely new insights.
Do NOT re-emit existing fixes."#,
        source_workflow_name
    );
    format!("{}\n{}", prompt, REFLECTION_GUARDRAILS)
}

/// Helper: Build a prompt step.
fn build_prompt_step(name: &str, content: &str) -> ExecutionStepConfig {
    ExecutionStepConfig {
        step_type: "prompt".to_string(),
        name: Some(name.to_string()),
        prompt_content: Some(content.to_string()),
        ..Default::default()
    }
}

/// Helper: Build a verification prompt step.
fn build_verification_prompt_step(name: &str, content: &str) -> ExecutionStepConfig {
    let mut step = build_prompt_step(name, content);
    step.phase = Some("verification".to_string());
    step
}
