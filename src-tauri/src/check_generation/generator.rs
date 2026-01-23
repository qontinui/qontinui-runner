//! Check Generation Module
//!
//! Uses AI to generate appropriate check configurations based on detected projects.

use tracing::{debug, info, warn};

use super::types::{
    CreateCheckInput, GenerateChecksRequest, GenerateChecksResponse, SuggestedCheckWithReason,
    UserPreferences, WorkspaceScanResult,
};
use crate::ai_provider;
use crate::ai_router::TaskContext;

/// Tool suggestions per language (used when AI fails or for quick suggestions)
struct ToolSuggestion {
    tool: &'static str,
    name: &'static str,
    check_type: &'static str,
    default_command: &'static str,
    supports_auto_fix: bool,
    reason: &'static str,
}

const PYTHON_TOOLS: &[ToolSuggestion] = &[
    ToolSuggestion {
        tool: "ruff",
        name: "Ruff Linting",
        check_type: "lint",
        default_command: "ruff check .",
        supports_auto_fix: true,
        reason: "Fast Python linter that catches common issues",
    },
    ToolSuggestion {
        tool: "black",
        name: "Black Formatting",
        check_type: "format",
        default_command: "black --check .",
        supports_auto_fix: true,
        reason: "Consistent Python code formatting",
    },
    ToolSuggestion {
        tool: "mypy",
        name: "MyPy Type Check",
        check_type: "typecheck",
        default_command: "mypy .",
        supports_auto_fix: false,
        reason: "Static type checking for Python",
    },
];

const TYPESCRIPT_TOOLS: &[ToolSuggestion] = &[
    ToolSuggestion {
        tool: "tsc",
        name: "TypeScript Compile",
        check_type: "typecheck",
        default_command: "npx tsc --noEmit",
        supports_auto_fix: false,
        reason: "TypeScript type checking",
    },
    ToolSuggestion {
        tool: "eslint",
        name: "ESLint",
        check_type: "lint",
        default_command: "npx eslint .",
        supports_auto_fix: true,
        reason: "JavaScript/TypeScript linting",
    },
    ToolSuggestion {
        tool: "prettier",
        name: "Prettier Formatting",
        check_type: "format",
        default_command: "npx prettier --check .",
        supports_auto_fix: true,
        reason: "Consistent code formatting",
    },
];

const JAVASCRIPT_TOOLS: &[ToolSuggestion] = &[
    ToolSuggestion {
        tool: "eslint",
        name: "ESLint",
        check_type: "lint",
        default_command: "npx eslint .",
        supports_auto_fix: true,
        reason: "JavaScript linting",
    },
    ToolSuggestion {
        tool: "prettier",
        name: "Prettier Formatting",
        check_type: "format",
        default_command: "npx prettier --check .",
        supports_auto_fix: true,
        reason: "Consistent code formatting",
    },
];

const RUST_TOOLS: &[ToolSuggestion] = &[
    ToolSuggestion {
        tool: "clippy",
        name: "Clippy Linting",
        check_type: "lint",
        default_command: "cargo clippy -- -D warnings",
        supports_auto_fix: true,
        reason: "Rust linting with helpful suggestions",
    },
    ToolSuggestion {
        tool: "rustfmt",
        name: "Rustfmt Formatting",
        check_type: "format",
        default_command: "cargo fmt --check",
        supports_auto_fix: true,
        reason: "Consistent Rust code formatting",
    },
    ToolSuggestion {
        tool: "cargo_check",
        name: "Cargo Check",
        check_type: "typecheck",
        default_command: "cargo check",
        supports_auto_fix: false,
        reason: "Fast Rust compilation check",
    },
];

/// Generate check suggestions using AI
pub fn generate_checks(request: &GenerateChecksRequest) -> GenerateChecksResponse {
    let preferences = request.user_preferences.clone().unwrap_or_default();

    // First try AI generation
    match generate_with_ai(&request.workspace_scan, &preferences) {
        Ok(checks) if !checks.is_empty() => {
            info!("AI generated {} check suggestions", checks.len());
            GenerateChecksResponse {
                suggested_checks: checks,
                success: true,
                error: None,
            }
        }
        Ok(_) => {
            warn!("AI returned empty suggestions, falling back to rule-based");
            generate_rule_based(&request.workspace_scan, &preferences)
        }
        Err(e) => {
            warn!("AI generation failed: {}, falling back to rule-based", e);
            generate_rule_based(&request.workspace_scan, &preferences)
        }
    }
}

/// Generate checks using AI
fn generate_with_ai(
    scan: &WorkspaceScanResult,
    preferences: &UserPreferences,
) -> Result<Vec<SuggestedCheckWithReason>, String> {
    let prompt = build_ai_prompt(scan, preferences);

    debug!("AI prompt length: {} chars", prompt.len());

    // Create task context for routing
    let context = TaskContext {
        prompt: prompt.clone(),
        file_count: Some(scan.projects.len()),
        criteria_count: None,
        file_paths: scan.projects.iter().map(|p| p.path.clone()).collect(),
    };

    // Run AI prompt
    let response = ai_provider::run_prompt_with_routing(&prompt, &context, 120);

    if !response.success {
        return Err(response
            .error
            .unwrap_or_else(|| "Unknown AI error".to_string()));
    }

    // Parse AI response
    parse_ai_response(&response.output, scan)
}

/// Build the AI prompt for check generation
fn build_ai_prompt(scan: &WorkspaceScanResult, preferences: &UserPreferences) -> String {
    let mut prompt = String::new();

    prompt.push_str("You are a code quality expert. Based on the following workspace scan results, suggest appropriate code quality checks.\n\n");

    // Add detected projects
    prompt.push_str("## Detected Projects\n\n");
    for project in &scan.projects {
        prompt.push_str(&format!(
            "### {}\n- Path: {}\n- Languages: {}\n- Config files: {}\n\n",
            project.name,
            project.path,
            project.languages.join(", "),
            if project.config_files.is_empty() {
                "none".to_string()
            } else {
                project.config_files.join(", ")
            }
        ));
    }

    // Add preferences
    prompt.push_str("## User Preferences\n");
    prompt.push_str(&format!(
        "- Prefer auto-fix: {}\n",
        preferences.prefer_auto_fix
    ));
    prompt.push_str(&format!(
        "- Include security checks: {}\n",
        preferences.include_security
    ));
    prompt.push_str(&format!(
        "- Include formatting checks: {}\n",
        preferences.include_formatting
    ));
    prompt.push_str(&format!(
        "- Include type checking: {}\n",
        preferences.include_type_checking
    ));

    // Add available tools reference
    prompt.push_str("\n## Available Tools\n");
    prompt.push_str("Python: black (format), isort (format), ruff (lint), mypy (typecheck), pyright (typecheck)\n");
    prompt.push_str("JavaScript/TypeScript: eslint (lint), prettier (format), tsc (typecheck), biome (lint/format)\n");
    prompt.push_str("Rust: clippy (lint), rustfmt (format), cargo_check (typecheck)\n");

    // Output format
    prompt.push_str("\n## Output Format\n");
    prompt
        .push_str("Respond with a JSON array of check suggestions. Each suggestion should have:\n");
    prompt.push_str("```json\n[\n  {\n");
    prompt.push_str("    \"check\": {\n");
    prompt.push_str("      \"name\": \"Check Name\",\n");
    prompt.push_str("      \"description\": \"What this check does\",\n");
    prompt.push_str("      \"check_type\": \"lint|format|typecheck|security|analyze\",\n");
    prompt.push_str("      \"tool\": \"tool_name\",\n");
    prompt.push_str("      \"command\": \"the command to run (optional, tool has default)\",\n");
    prompt.push_str("      \"working_directory\": \"/absolute/path/to/project\",\n");
    prompt.push_str("      \"auto_fix\": true|false,\n");
    prompt.push_str("      \"timeout_seconds\": 60,\n");
    prompt.push_str("      \"is_critical\": false,\n");
    prompt.push_str("      \"tags\": [\"tag1\", \"tag2\"]\n");
    prompt.push_str("    },\n");
    prompt.push_str("    \"reason\": \"Why this check is recommended\",\n");
    prompt.push_str("    \"project_path\": \"/absolute/path/to/project\"\n");
    prompt.push_str("  }\n]\n```\n\n");

    prompt.push_str("IMPORTANT:\n");
    prompt.push_str("- Set working_directory to the ABSOLUTE project path for each check\n");
    prompt.push_str("- Use standard tool names: black, ruff, mypy, eslint, prettier, tsc, clippy, rustfmt, cargo_check\n");
    prompt.push_str(
        "- Only suggest checks for languages that are actually detected in each project\n",
    );
    prompt.push_str("- If a config file exists (e.g., pyproject.toml, eslint.config.js), the tool will use it automatically\n");
    prompt.push_str("- Respond ONLY with the JSON array, no other text\n");

    prompt
}

/// Parse AI response into structured check suggestions
fn parse_ai_response(
    response: &str,
    scan: &WorkspaceScanResult,
) -> Result<Vec<SuggestedCheckWithReason>, String> {
    // Extract JSON from response (AI might include markdown code blocks)
    let json_str = extract_json_array(response)?;

    // Parse JSON
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&json_str)
        .map_err(|e| format!("Failed to parse AI response as JSON: {}", e))?;

    let mut suggestions = Vec::new();

    for item in parsed {
        match parse_suggestion_item(&item, scan) {
            Ok(suggestion) => suggestions.push(suggestion),
            Err(e) => {
                warn!("Failed to parse suggestion item: {}", e);
                // Continue with other items
            }
        }
    }

    Ok(suggestions)
}

/// Extract JSON array from response that might be wrapped in markdown
fn extract_json_array(response: &str) -> Result<String, String> {
    let response = response.trim();

    // If it starts with [, assume it's raw JSON
    if response.starts_with('[') {
        // Find the matching closing bracket
        let mut depth = 0;
        let mut end_idx = 0;
        for (i, c) in response.char_indices() {
            match c {
                '[' => depth += 1,
                ']' => {
                    depth -= 1;
                    if depth == 0 {
                        end_idx = i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        if end_idx > 0 {
            return Ok(response[..end_idx].to_string());
        }
        return Ok(response.to_string());
    }

    // Try to extract from markdown code block
    if let Some(start) = response.find("```json") {
        let json_start = start + 7;
        if let Some(end) = response[json_start..].find("```") {
            return Ok(response[json_start..json_start + end].trim().to_string());
        }
    }

    // Try to extract from generic code block
    if let Some(start) = response.find("```") {
        let block_start = start + 3;
        // Skip language identifier if present
        let content_start = response[block_start..]
            .find('\n')
            .map(|i| block_start + i + 1)
            .unwrap_or(block_start);
        if let Some(end) = response[content_start..].find("```") {
            return Ok(response[content_start..content_start + end]
                .trim()
                .to_string());
        }
    }

    // Try to find array anywhere in response
    if let Some(start) = response.find('[') {
        if let Some(end) = response.rfind(']') {
            if end > start {
                return Ok(response[start..=end].to_string());
            }
        }
    }

    Err("Could not find JSON array in AI response".to_string())
}

/// Parse a single suggestion item from JSON
fn parse_suggestion_item(
    item: &serde_json::Value,
    _scan: &WorkspaceScanResult,
) -> Result<SuggestedCheckWithReason, String> {
    let check_obj = item
        .get("check")
        .ok_or("Missing 'check' field in suggestion")?;

    let name = check_obj
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'name' in check")?
        .to_string();

    let check_type = check_obj
        .get("check_type")
        .and_then(|v| v.as_str())
        .unwrap_or("lint")
        .to_string();

    let tool = check_obj
        .get("tool")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'tool' in check")?
        .to_string();

    let working_directory = check_obj
        .get("working_directory")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let reason = item
        .get("reason")
        .and_then(|v| v.as_str())
        .unwrap_or("AI suggested")
        .to_string();

    let project_path = item
        .get("project_path")
        .and_then(|v| v.as_str())
        .or_else(|| working_directory.as_deref())
        .unwrap_or("")
        .to_string();

    let check = CreateCheckInput {
        name,
        description: check_obj
            .get("description")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        check_type,
        tool,
        command: check_obj
            .get("command")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        working_directory,
        config_path: check_obj
            .get("config_path")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        auto_fix: check_obj
            .get("auto_fix")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        fail_on_warning: check_obj
            .get("fail_on_warning")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        timeout_seconds: check_obj
            .get("timeout_seconds")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32)
            .unwrap_or(60),
        is_critical: check_obj
            .get("is_critical")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        enabled: true,
        ai_generated: true,
        ai_generation_prompt: Some("Generated from workspace scan".to_string()),
        tags: check_obj
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default(),
    };

    Ok(SuggestedCheckWithReason {
        check,
        reason,
        project_path,
    })
}

/// Generate checks using rule-based approach (fallback when AI fails)
fn generate_rule_based(
    scan: &WorkspaceScanResult,
    preferences: &UserPreferences,
) -> GenerateChecksResponse {
    let mut suggestions = Vec::new();

    for project in &scan.projects {
        for language in &project.languages {
            let tools = match language.as_str() {
                "python" => PYTHON_TOOLS,
                "typescript" => TYPESCRIPT_TOOLS,
                "javascript" => JAVASCRIPT_TOOLS,
                "rust" => RUST_TOOLS,
                _ => continue,
            };

            for tool in tools {
                // Apply preferences
                if !preferences.include_formatting && tool.check_type == "format" {
                    continue;
                }
                if !preferences.include_type_checking && tool.check_type == "typecheck" {
                    continue;
                }

                let check = CreateCheckInput {
                    name: format!("{} - {}", project.name, tool.name),
                    description: Some(tool.reason.to_string()),
                    check_type: tool.check_type.to_string(),
                    tool: tool.tool.to_string(),
                    command: Some(tool.default_command.to_string()),
                    working_directory: Some(project.path.clone()),
                    config_path: None,
                    auto_fix: preferences.prefer_auto_fix && tool.supports_auto_fix,
                    fail_on_warning: false,
                    timeout_seconds: 60,
                    is_critical: false,
                    enabled: true,
                    ai_generated: true,
                    ai_generation_prompt: Some(
                        "Generated from workspace scan (rule-based)".to_string(),
                    ),
                    tags: vec![language.clone(), tool.check_type.to_string()],
                };

                suggestions.push(SuggestedCheckWithReason {
                    check,
                    reason: tool.reason.to_string(),
                    project_path: project.path.clone(),
                });
            }
        }
    }

    info!(
        "Rule-based generation produced {} check suggestions",
        suggestions.len()
    );

    GenerateChecksResponse {
        suggested_checks: suggestions,
        success: true,
        error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_json_array_raw() {
        let response = r#"[{"check": {"name": "Test"}}]"#;
        let result = extract_json_array(response).unwrap();
        assert!(result.starts_with('['));
    }

    #[test]
    fn test_extract_json_array_markdown() {
        let response = r#"Here are the suggestions:
```json
[{"check": {"name": "Test"}}]
```
"#;
        let result = extract_json_array(response).unwrap();
        assert!(result.starts_with('['));
    }

    #[test]
    fn test_extract_json_array_mixed() {
        let response = "Some text before [1, 2, 3] and after";
        let result = extract_json_array(response).unwrap();
        assert_eq!(result, "[1, 2, 3]");
    }
}
