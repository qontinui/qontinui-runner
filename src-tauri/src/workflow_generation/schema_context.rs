//! Schema Context for Workflow Generation
//!
//! Builds the AI prompt with workflow schema documentation and examples.

/// Build the complete schema context prompt for AI workflow generation.
pub fn build_schema_context() -> String {
    format!(
        r#"You are a workflow generation assistant for Qontinui Runner.

## Your Task
Generate a valid UnifiedWorkflow JSON based on the user's description.
Return ONLY valid JSON. No markdown code blocks, no explanations, just raw JSON.

## Workflow Structure

A UnifiedWorkflow executes in 4 phases:
1. **Setup** - Runs ONCE at the beginning (environment preparation)
2. **Verification** - Runs in loop with Agentic (success checks)
3. **Agentic** - AI prompts only (corrective actions when verification fails)
4. **Completion** - Runs ONCE at the end (cleanup, notifications)

Execution Order: Setup (once) -> [Verification <-> Agentic]* -> Completion (once)

The Verification/Agentic loop continues until all blocking checks pass or max_iterations is reached.

## UnifiedWorkflow Schema

```json
{{
  "id": "uuid-v4",
  "name": "string (required)",
  "description": "string (required)",
  "setup_steps": [...],
  "verification_steps": [...],
  "agentic_steps": [...],
  "completion_steps": [...],
  "max_iterations": number (default: 10),
  "category": "string (e.g., 'testing', 'development', 'deployment')",
  "tags": ["string"],
  "created_at": "ISO 8601 timestamp",
  "modified_at": "ISO 8601 timestamp"
}}
```

## Step Types

### Common Fields (all steps)
- `id`: UUID v4 string (required)
- `name`: Display name (required)
- `phase`: Which phase this step belongs to (required)

{step_types}

## Phase Constraints

| Step Type | setup | verification | agentic | completion |
|-----------|-------|--------------|---------|------------|
| script | ✓ | | | ✓ |
| state | ✓ | ✓ | | |
| workflow_ref | ✓ | ✓ | | |
| gui_action | ✓ | ✓ | | |
| api_request | ✓ | ✓ | | ✓ |
| mcp_call | ✓ | ✓ | | ✓ |
| test | | ✓ | | |
| check | | ✓ | | |
| screenshot | | ✓ | | |
| prompt | ✓ | ✓ | ✓ | ✓ |
| shell_command | ✓ | | | ✓ |
| awas_discover | ✓ | | | |
| awas_execute | ✓ | ✓ | | |
| awas_check_support | ✓ | | | |
| awas_list_actions | ✓ | ✓ | | |
| awas_extract_elements | | ✓ | | |

## Examples

{examples}

## Important Rules
1. Generate valid UUIDs for all `id` fields (format: xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx)
2. `phase` field MUST match the array the step is in (setup_steps -> "setup", etc.)
3. `agentic_steps` can ONLY contain `prompt` type steps
4. Use ISO 8601 format for timestamps (e.g., "2024-01-15T10:30:00Z")
5. Return ONLY the JSON object, no markdown formatting"#,
        step_types = get_step_types_documentation(),
        examples = get_example_workflows()
    )
}

fn get_step_types_documentation() -> &'static str {
    r#"
### script (Setup or Completion)
Playwright browser automation script.
```json
{
  "type": "script",
  "phase": "setup" | "completion",
  "code": "string (Playwright TypeScript code)",
  "target_url": "string (starting URL, optional)",
  "refinement_enabled": boolean (default: true)
}
```

### test (Verification only)
Runs verification checks.
```json
{
  "type": "test",
  "phase": "verification",
  "test_type": "playwright" | "qontinui_vision" | "python" | "repository" | "custom_command",
  "command": "string (for repository/custom_command)",
  "code": "string (for playwright/python)",
  "description": "string"
}
```

### check (Verification only)
Code quality checks (lint, format, typecheck).
```json
{
  "type": "check",
  "phase": "verification",
  "check_type": "lint" | "format" | "typecheck" | "analyze" | "security" | "custom_command",
  "command": "string (command to run)",
  "working_directory": "string (optional)",
  "auto_fix": boolean (default: false)
}
```

### prompt (Any phase)
AI task instructions.
```json
{
  "type": "prompt",
  "phase": "setup" | "verification" | "agentic" | "completion",
  "content": "string (the prompt instructions)"
}
```

### shell_command (Setup or Completion)
Shell command execution.
```json
{
  "type": "shell_command",
  "phase": "setup" | "completion",
  "command": "string (shell command)",
  "working_directory": "string (optional)",
  "timeout_seconds": number (default: 60),
  "fail_on_error": boolean (default: true)
}
```

### api_request (Setup, Verification, or Completion)
HTTP API calls with variable extraction.
```json
{
  "type": "api_request",
  "phase": "setup" | "verification" | "completion",
  "method": "GET" | "POST" | "PUT" | "PATCH" | "DELETE",
  "url": "string",
  "headers": { "key": "value" },
  "body": "string (optional)",
  "content_type": "application/json" | "text/plain" | "none",
  "extractions": [{ "variable_name": "string", "json_path": "$.path" }],
  "assertions": [{ "type": "status_code", "expected": 200 }]
}
```

### screenshot (Verification only)
Captures screen state for AI analysis.
```json
{
  "type": "screenshot",
  "phase": "verification",
  "delay_ms": number (optional),
  "monitor": "all" | "primary" | "left" | "right" | number
}
```

### gui_action (Setup or Verification)
Mouse and keyboard automation.
```json
{
  "type": "gui_action",
  "phase": "setup" | "verification",
  "action": "click" | "double_click" | "right_click" | "type" | "hotkey" | "scroll",
  "target_image_ids": ["uuid"],
  "text_input": "string (for type action)",
  "hotkey": "string (e.g., 'ctrl+s')",
  "scroll_direction": "up" | "down"
}
```

### state (Setup or Verification)
Navigate to stored application state.
```json
{
  "type": "state",
  "phase": "setup" | "verification",
  "state_id": "uuid",
  "state_name": "string (display name)",
  "timeout_seconds": number (optional)
}
```

### workflow_ref (Setup or Verification)
Execute another workflow.
```json
{
  "type": "workflow_ref",
  "phase": "setup" | "verification",
  "workflow_id": "uuid",
  "workflow_name": "string (display name)"
}
```

### mcp_call (Setup, Verification, or Completion)
Call MCP server tool.
```json
{
  "type": "mcp_call",
  "phase": "setup" | "verification" | "completion",
  "server_id": "string",
  "tool_name": "string",
  "arguments": { "key": "value" }
}
```"#
}

fn get_example_workflows() -> &'static str {
    r#"
### Example 1: TypeScript Type Check and Fix
User request: "Run TypeScript type checking and fix any errors"
```json
{
  "id": "a1b2c3d4-e5f6-4a7b-8c9d-0e1f2a3b4c5d",
  "name": "TypeScript Type Check",
  "description": "Run TypeScript type checking and automatically fix type errors",
  "setup_steps": [],
  "verification_steps": [
    {
      "id": "b2c3d4e5-f6a7-4b8c-9d0e-1f2a3b4c5d6e",
      "type": "check",
      "phase": "verification",
      "name": "TypeScript Type Check",
      "check_type": "typecheck",
      "command": "npx tsc --noEmit"
    }
  ],
  "agentic_steps": [
    {
      "id": "c3d4e5f6-a7b8-4c9d-0e1f-2a3b4c5d6e7f",
      "type": "prompt",
      "phase": "agentic",
      "name": "Fix Type Errors",
      "content": "Fix the TypeScript type errors found in the verification step. Read the error messages, understand the type issues, and make the necessary corrections to the code."
    }
  ],
  "completion_steps": [],
  "max_iterations": 5,
  "category": "development",
  "tags": ["typescript", "types", "quality"],
  "created_at": "2024-01-15T10:00:00Z",
  "modified_at": "2024-01-15T10:00:00Z"
}
```

### Example 2: Run Tests with Pre-Setup
User request: "Install dependencies, run pytest, and fix failing tests"
```json
{
  "id": "d4e5f6a7-b8c9-4d0e-1f2a-3b4c5d6e7f8a",
  "name": "Python Test Suite",
  "description": "Install dependencies, run pytest, and fix any failing tests",
  "setup_steps": [
    {
      "id": "e5f6a7b8-c9d0-4e1f-2a3b-4c5d6e7f8a9b",
      "type": "shell_command",
      "phase": "setup",
      "name": "Install Dependencies",
      "command": "pip install -r requirements.txt",
      "fail_on_error": true
    }
  ],
  "verification_steps": [
    {
      "id": "f6a7b8c9-d0e1-4f2a-3b4c-5d6e7f8a9b0c",
      "type": "test",
      "phase": "verification",
      "name": "Run Pytest",
      "test_type": "repository",
      "command": "pytest -v"
    }
  ],
  "agentic_steps": [
    {
      "id": "a7b8c9d0-e1f2-4a3b-4c5d-6e7f8a9b0c1d",
      "type": "prompt",
      "phase": "agentic",
      "name": "Fix Failing Tests",
      "content": "Analyze the pytest output and fix the failing tests. Make necessary code changes to ensure all tests pass."
    }
  ],
  "completion_steps": [
    {
      "id": "b8c9d0e1-f2a3-4b4c-5d6e-7f8a9b0c1d2e",
      "type": "prompt",
      "phase": "completion",
      "name": "Summary",
      "content": "Summarize all the changes made to fix the failing tests."
    }
  ],
  "max_iterations": 10,
  "category": "testing",
  "tags": ["python", "pytest", "testing"],
  "created_at": "2024-01-15T10:00:00Z",
  "modified_at": "2024-01-15T10:00:00Z"
}
```

### Example 3: Build and Lint Check
User request: "Run ESLint and Prettier on my React project"
```json
{
  "id": "c9d0e1f2-a3b4-4c5d-6e7f-8a9b0c1d2e3f",
  "name": "React Code Quality",
  "description": "Run ESLint and Prettier checks with auto-fix",
  "setup_steps": [],
  "verification_steps": [
    {
      "id": "d0e1f2a3-b4c5-4d6e-7f8a-9b0c1d2e3f4a",
      "type": "check",
      "phase": "verification",
      "name": "ESLint Check",
      "check_type": "lint",
      "command": "npx eslint src --ext .ts,.tsx",
      "auto_fix": true
    },
    {
      "id": "e1f2a3b4-c5d6-4e7f-8a9b-0c1d2e3f4a5b",
      "type": "check",
      "phase": "verification",
      "name": "Prettier Check",
      "check_type": "format",
      "command": "npx prettier --check src",
      "auto_fix": true
    }
  ],
  "agentic_steps": [
    {
      "id": "f2a3b4c5-d6e7-4f8a-9b0c-1d2e3f4a5b6c",
      "type": "prompt",
      "phase": "agentic",
      "name": "Fix Remaining Issues",
      "content": "Fix any ESLint or Prettier issues that couldn't be auto-fixed. Read the error messages and make the necessary code changes."
    }
  ],
  "completion_steps": [],
  "max_iterations": 3,
  "category": "quality",
  "tags": ["eslint", "prettier", "react", "linting"],
  "created_at": "2024-01-15T10:00:00Z",
  "modified_at": "2024-01-15T10:00:00Z"
}
```"#
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_schema_context() {
        let context = build_schema_context();
        assert!(context.contains("UnifiedWorkflow"));
        assert!(context.contains("setup_steps"));
        assert!(context.contains("verification_steps"));
        assert!(context.contains("agentic_steps"));
        assert!(context.contains("completion_steps"));
    }
}
