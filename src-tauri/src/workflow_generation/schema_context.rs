//! Schema Context for Workflow Generation
//!
//! Builds the AI prompt with workflow schema documentation and examples.
//! Documentation is auto-generated from the step type metadata registry
//! rather than hardcoded static strings.

use rusqlite::Connection;

use super::example_workflows::{find_relevant_examples, format_examples_for_prompt};
use super::relevance_filter::filter_relevant_step_types;
use super::rules;
use super::step_type_metadata::{get_all_step_type_metadata, StepTypeMetadata};

/// Build the complete schema context prompt for AI workflow generation.
///
/// Backward-compatible: uses all step types and no RAG examples.
pub fn build_schema_context() -> String {
    let all_types = get_all_step_type_metadata();
    let type_refs: Vec<&StepTypeMetadata> = all_types.iter().collect();
    let step_types_doc = generate_step_types_documentation(&type_refs);
    let phase_table = generate_phase_constraint_table(&type_refs);
    assemble_prompt(&step_types_doc, &phase_table, "", None)
}

/// Build schema context filtered by description keywords.
///
/// Reduces token usage by only including step types relevant to the description.
/// Uses no RAG examples (no DB access).
pub fn build_schema_context_for_description(description: &str) -> String {
    let all_types = get_all_step_type_metadata();
    let filtered = filter_relevant_step_types(description, all_types);
    let step_types_doc = generate_step_types_documentation(&filtered);
    let phase_table = generate_phase_constraint_table(&filtered);
    assemble_prompt(&step_types_doc, &phase_table, "", None)
}

/// Build full schema context with filtered types + RAG examples from DB.
///
/// This is the most complete version, used when a DB connection and optionally
/// a query embedding are available.
pub fn build_schema_context_full(
    description: &str,
    conn: Option<&Connection>,
    query_embedding: Option<&[f32]>,
) -> String {
    let all_types = get_all_step_type_metadata();
    let filtered = filter_relevant_step_types(description, all_types);
    let step_types_doc = generate_step_types_documentation(&filtered);
    let phase_table = generate_phase_constraint_table(&filtered);

    // Retrieve RAG examples if DB is available
    let examples_section = if let Some(conn) = conn {
        let examples = find_relevant_examples(conn, query_embedding, None, 3);
        if !examples.is_empty() {
            format_examples_for_prompt(&examples, 3)
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    assemble_prompt(&step_types_doc, &phase_table, &examples_section, conn)
}

// ============================================================================
// Auto-Generated Documentation
// ============================================================================

/// Generate the step types documentation section from metadata.
///
/// Produces markdown blocks like:
/// ```text
/// ### shell_command (Setup or Completion)
/// Execute a shell command.
/// Fields:
/// - `command`: string (required) — Shell command to execute
/// ```
pub fn generate_step_types_documentation(types: &[&StepTypeMetadata]) -> String {
    let mut output = String::with_capacity(4096);

    for meta in types {
        // Header: ### type_name (phases)
        let phases_str = format_phases_for_header(meta.allowed_phases);
        output.push_str(&format!(
            "### {} ({})\n{}\n```json\n{{\n  \"type\": \"{}\",\n  \"phase\": {},\n",
            meta.step_type, phases_str, meta.description, meta.step_type,
            format_phase_values(meta.allowed_phases),
        ));

        // Add fields
        for field in meta.fields {
            let type_str = if !field.enum_values.is_empty() {
                field
                    .enum_values
                    .iter()
                    .map(|v| format!("\"{}\"", v))
                    .collect::<Vec<_>>()
                    .join(" | ")
            } else {
                field.field_type.as_str().to_string()
            };

            let default_str = if !field.default.is_empty() {
                format!(" (default: {})", field.default)
            } else {
                String::new()
            };

            output.push_str(&format!(
                "  \"{}\": {}{}\n",
                field.name, type_str, default_str,
            ));
        }

        output.push_str("}\n```\n\n");
    }

    output
}

/// Generate the phase constraint table from metadata.
pub fn generate_phase_constraint_table(types: &[&StepTypeMetadata]) -> String {
    let mut output = String::with_capacity(2048);
    output.push_str("| Step Type | setup | verification | agentic | completion |\n");
    output.push_str("|-----------|-------|--------------|---------|------------|\n");

    for meta in types {
        let setup = if meta.allowed_phases.contains(&"setup") {
            "✓"
        } else {
            ""
        };
        let verification = if meta.allowed_phases.contains(&"verification") {
            "✓"
        } else {
            ""
        };
        let agentic = if meta.allowed_phases.contains(&"agentic") {
            "✓"
        } else {
            ""
        };
        let completion = if meta.allowed_phases.contains(&"completion") {
            "✓"
        } else {
            ""
        };

        output.push_str(&format!(
            "| {} | {} | {} | {} | {} |\n",
            meta.step_type, setup, verification, agentic, completion
        ));
    }

    output
}

// ============================================================================
// Prompt Assembly
// ============================================================================

fn assemble_prompt(step_types: &str, phase_table: &str, examples: &str, conn: Option<&Connection>) -> String {
    let examples_section = if examples.is_empty() {
        String::new()
    } else {
        format!("## Examples\n\n{}", examples)
    };

    // Build rules from DB if available, otherwise use hardcoded fallback
    let rules_section = build_rules_section(conn);

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

{phase_table}

{examples_section}

{rules_section}

## Environment Aliases
When the user mentions these terms, map them to the correct endpoints:
- "runner" / "desktop app" → Qontinui Runner at http://localhost:1420 (UI) / http://localhost:9876 (API)
- "web" / "web app" / "frontend" → Qontinui Web at http://localhost:3001
- "backend" / "API" / "server" → Web backend at http://localhost:8000
- Use health check API requests to verify services are running before testing

## UI Bridge SDK (REQUIRED for Web App Verification)

When a workflow targets a web application (localhost:3001, localhost:1420, or any React/Next.js app),
you MUST use the UI Bridge SDK for verification steps. The SDK provides direct programmatic access
to registered UI elements AND page content without browser automation overhead.
Playwright may be used as a SUPPLEMENT but is NOT a substitute for SDK-based verification when the SDK is available.

### How the SDK Works

The UI Bridge SDK is installed globally at the application level — it instruments ALL pages automatically.
The SDK's `UIBridgeProvider` and `AutoRegisterProvider` are mounted in the root layout and use a
`MutationObserver` to auto-discover every interactive and content element on every page. No per-page
or per-component setup is needed.

### How Snapshots Work

The runner provides TWO snapshot paths:
1. **Control snapshot** (`GET /ui-bridge/control/snapshot`) — Captures the runner's OWN React frontend (Tauri webview). No setup needed.
2. **SDK snapshot** (`GET /ui-bridge/sdk/snapshot`) — Captures an EXTERNAL SDK-integrated app via HTTP proxy. **Requires `sdk_connect` first.**

The SDK snapshot is a proxy: the runner forwards your request to the connected app's `/api/ui-bridge/control/snapshot` endpoint. If no app is connected, the snapshot fails — NOT because the page lacks SDK instrumentation, but because the proxy has no target.

### Setup: Connect Before Snapshot

Every workflow that uses SDK snapshots MUST include a connect step in setup:
```json
{{
  "type": "api_request",
  "phase": "setup",
  "name": "Connect UI Bridge SDK",
  "method": "POST",
  "url": "http://localhost:9876/ui-bridge/sdk/connect",
  "body": "{{\"url\": \"http://localhost:3001\"}}",
  "content_type": "application/json",
  "assertions": [{{"type": "status_code", "expected": 200}}]
}}
```
If this succeeds, the target app has the SDK installed and all SDK endpoints become available.

**IMPORTANT:** After connecting, the snapshot returns whatever page the user's browser is currently showing.
To verify a specific page, navigate first using `POST /ui-bridge/sdk/page/navigate` with the target URL,
then take the snapshot. The SDK instruments ALL pages — the snapshot will work on any page the browser navigates to.

### SDK Endpoints (via Runner API at localhost:9876)
After connecting, these endpoints are available:

**Elements & Snapshots:**
- `GET /ui-bridge/sdk/elements` — List all elements (interactive + content). Supports filters: `?contentOnly=true`, `?contentTypes=heading,metric-value`
- `GET /ui-bridge/sdk/snapshot` — Full UI snapshot (all elements + state). Supports `?includeContent=false` to exclude content
- `GET /ui-bridge/sdk/element/{{id}}` — Get specific element details
- `GET /ui-bridge/sdk/components` — List registered components
- `POST /ui-bridge/sdk/discover` — Find elements by criteria. Body: `{{"interactive_only": false, "includeContent": true, "contentRoles": ["heading", "metric"]}}`

**Actions:**
- `POST /ui-bridge/sdk/element/{{id}}/action` — Execute action (click, type, focus, hover)

**Page Navigation:**
- `POST /ui-bridge/sdk/page/refresh` — Refresh the current page
- `POST /ui-bridge/sdk/page/navigate` — Navigate to URL. Body: `{{"url": "http://..."}}`
- `POST /ui-bridge/sdk/page/back` — Go back in browser history
- `POST /ui-bridge/sdk/page/forward` — Go forward in browser history

**AI-Powered:**
- `POST /ui-bridge/sdk/ai/search` — Search elements by description. Body: `{{"text": "submit button", "contentRole": "badge"}}`
- `POST /ui-bridge/sdk/ai/execute` — Execute action by description. Body: `{{"instruction": "click the Submit button"}}`
- `GET /ui-bridge/sdk/ai/summary` — AI-friendly page summary
- `GET /ui-bridge/sdk/ai/snapshot` — Semantic snapshot for analysis
- `GET /ui-bridge/sdk/ai/analyze/data` — Extract labeled data values
- `GET /ui-bridge/sdk/ai/analyze/regions` — Segment page into semantic regions
- `GET /ui-bridge/sdk/ai/analyze/structured-data` — Extract tables and lists
- `POST /ui-bridge/sdk/ai/analyze/cross-app-compare` — Compare two app snapshots (includes content comparison)

### SDK Response Schemas (for writing meaningful assertions)

**CRITICAL:** A `status_code: 200` from an SDK endpoint does NOT prove the UI is correct.
Many SDK endpoints return 200 with empty results (e.g., `ai/search` returns `{{"results": []}}` when nothing matches).
Always add `body_contains` or `json_path` assertions to verify the response has meaningful content.

**`POST /ui-bridge/sdk/ai/search`** — Returns matched elements:
```json
{{"results": [{{"id": "element-id", "type": "button", "text": "Submit", "score": 0.95}}], "total": 1}}
```
- On no match: `{{"results": [], "total": 0}}` (still 200!)
- Good assertion: `body_contains` with the expected element text or `json_path: "$.total"` with `operator: ">"`, `expected: "0"`

**`GET /ui-bridge/sdk/elements`** — Returns element list:
```json
{{"elements": [{{"id": "...", "type": "button", "tagName": "BUTTON", "state": {{"textContent": "Save"}}}}], "total": 42}}
```
- Good assertion: `body_contains` with a known element ID or text content

**`GET /ui-bridge/sdk/snapshot`** — Returns full page state:
```json
{{"elements": [...], "metadata": {{"url": "http://...", "title": "Page Title"}}, "timestamp": "..."}}
```
- Good assertion: `body_contains` with expected page title or element content

**`POST /ui-bridge/sdk/discover`** — Returns discovered elements:
```json
{{"elements": [...], "total": 5}}
```
- Good assertion: `body_contains` with expected element attributes

### Content Discovery
The SDK automatically discovers **content elements** in addition to interactive elements.
Content elements include headings, paragraphs, labels, metrics, badges, status messages, and more.

Each content element has:
- `contentType`: heading, paragraph, label, metric-value, badge, status-message, description-text, list-item, table-cell, table-header, caption, blockquote, code-block, nav-text, content-generic
- `contentMetadata.contentRole`: heading, body-text, label, metric, badge, status, description, navigation, etc.
- `state.textContent`: The actual text content of the element

This means workflows can **read page text without screenshots**:
- Verify a metric shows the correct value by filtering for `contentType=metric-value`
- Check that a status badge says "Completed" by filtering for `contentRole=badge`
- Read all headings on a page to verify navigation worked
- Compare content between two apps using cross-app comparison

### When to Use UI Bridge vs Playwright
- **UI Bridge SDK**: Element inspection, state checking, clicking/typing on registered elements,
  **reading page text content**, verifying metric values, checking status indicators,
  navigation analysis, form validation, page structure analysis, page navigation (refresh/back/forward).
  Works on ALL pages of an SDK-integrated app — no per-page setup required.
- **Playwright**: Full browser testing, visual regression, complex multi-page flows,
  testing apps WITHOUT the SDK, screenshot-based verification, pixel-level checks

### SDK in Verification Steps
Use `api_request` steps to query SDK endpoints for verification:
```json
{{
  "type": "api_request",
  "phase": "verification",
  "name": "Check Page Content",
  "method": "GET",
  "url": "http://localhost:9876/ui-bridge/sdk/elements?contentOnly=true",
  "assertions": [{{"type": "status_code", "expected": 200}}]
}}
```

### SDK in Prompt Steps
In agentic/prompt steps, the AI agent has access to SDK MCP tools:
- `sdk_connect` — Connect to a target app
- `sdk_elements` — List all elements (interactive + content). Filters: contentOnly, contentTypes
- `sdk_snapshot` — Full UI snapshot with content elements. Filter: includeContent
- `sdk_discover` — Discover elements with content role filters
- `sdk_ai_search` — AI-powered element search by description. Filters: contentRole, contentTypes
- `sdk_ai_execute` — AI-powered action execution by description
- `sdk_page_refresh` — Refresh the current page
- `sdk_page_navigate` — Navigate to a URL
- `sdk_page_go_back` — Go back in browser history
- `sdk_page_go_forward` — Go forward in browser history
These are preferred over Playwright for inspection, content reading, and simple interactions."#,
        step_types = step_types,
        phase_table = phase_table,
        examples_section = examples_section,
    )
}

// ============================================================================
// Helpers
// ============================================================================

/// Build the Important Rules + Verification Quality Rules section.
///
/// Loads from `generation_rules` DB table if a connection is available,
/// otherwise falls back to the hardcoded text.
fn build_rules_section(conn: Option<&Connection>) -> String {
    if let Some(conn) = conn {
        let important = rules::load_rules(conn, "schema_context", "important_rules");
        let quality = rules::load_rules(conn, "schema_context", "verification_quality");

        // Only use DB rules if we actually got results (table might not be seeded yet)
        if !important.is_empty() || !quality.is_empty() {
            let mut s = String::from("## Important Rules\n");
            s.push_str(&rules::format_rules_as_markdown(&important));
            s.push_str("\n\n## Verification Quality Rules (MANDATORY)\n\nThese rules are NON-NEGOTIABLE. Workflows that violate them will be rejected.\n\n");
            s.push_str(&rules::format_rules_as_markdown(&quality));
            return s;
        }
    }

    // Fallback: hardcoded rules
    FALLBACK_SCHEMA_CONTEXT_RULES.to_string()
}

/// Hardcoded fallback rules used when no DB connection is available.
const FALLBACK_SCHEMA_CONTEXT_RULES: &str = r#"## Important Rules
1. **Generate valid UUIDs**: Generate valid UUIDs for all `id` fields (format: xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx)
2. **Phase field must match array**: `phase` field MUST match the array the step is in (setup_steps -> "setup", etc.)
3. **Agentic steps are prompt only**: `agentic_steps` can ONLY contain `prompt` type steps
4. **ISO 8601 timestamps**: Use ISO 8601 format for timestamps (e.g., "2024-01-15T10:30:00Z")
5. **JSON only output**: Return ONLY the JSON object, no markdown formatting

## Verification Quality Rules (MANDATORY)

These rules are NON-NEGOTIABLE. Workflows that violate them will be rejected.

6. **Deterministic verification step required**: verification_steps MUST include at least one deterministic, automated step — one of: `check`, `test`, `spec`, or `api_request` with assertions. Do NOT use only `prompt` type steps for verification.
7. **Code modification requires typecheck**: When the workflow creates or modifies source code files, verification MUST include a `check` step with the appropriate type checker.
8. **Web app verification requires SDK or Playwright**: When the workflow targets a web application, verification MUST include SDK-based or Playwright-based checks.
9. **Gate step required with ALL non-prompt steps**: Every workflow with 2+ verification steps MUST include a `gate` step whose `required_steps` array lists the IDs of ALL non-gate, non-prompt verification steps.
10. **Prompts are supplementary**: `prompt` type steps in verification must NEVER be the sole verification mechanism.
11. **Test steps with inline commands use repository**: When a `test` step runs a shell command, set `test_type: "repository"`.
12. **Next.js App Router path conventions**: Use correct paths like `src/app/(app)/`.
13. **Verification step caching**: Running tasks cache verification steps at startup; API updates don't affect current task.
14. **Feature-specific verification required**: Must include feature-specific checks, not just compilation/lint gates.
15. **SDK assertions must verify content**: A `status_code: 200` assertion alone is INSUFFICIENT for SDK endpoints.
16. **Agentic-verification correspondence**: Each agentic step must have a corresponding deterministic verification step."#;

// ============================================================================

fn format_phases_for_header(phases: &[&str]) -> String {
    if phases.len() == 4 {
        return "Any phase".to_string();
    }
    let labels: Vec<&str> = phases
        .iter()
        .map(|p| match *p {
            "setup" => "Setup",
            "verification" => "Verification",
            "agentic" => "Agentic",
            "completion" => "Completion",
            _ => p,
        })
        .collect();
    labels.join(" or ")
}

fn format_phase_values(phases: &[&str]) -> String {
    if phases.len() == 1 {
        format!("\"{}\"", phases[0])
    } else {
        let quoted: Vec<String> = phases.iter().map(|p| format!("\"{}\"", p)).collect();
        quoted.join(" | ")
    }
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

    #[test]
    fn test_build_schema_context_contains_step_types() {
        let context = build_schema_context();
        assert!(context.contains("### shell_command"));
        assert!(context.contains("### prompt"));
        assert!(context.contains("### check"));
        assert!(context.contains("### test"));
        assert!(context.contains("### api_request"));
    }

    #[test]
    fn test_build_schema_context_contains_phase_table() {
        let context = build_schema_context();
        assert!(context.contains("| Step Type | setup | verification | agentic | completion |"));
        assert!(context.contains("| shell_command |"));
    }

    #[test]
    fn test_filtered_context_smaller_than_full() {
        let full = build_schema_context();
        let filtered = build_schema_context_for_description("run pytest and fix errors");
        // Filtered should be smaller (no GUI, no AWAS types)
        assert!(
            filtered.len() < full.len(),
            "Filtered context ({}) should be smaller than full ({})",
            filtered.len(),
            full.len()
        );
    }

    #[test]
    fn test_filtered_context_still_has_core_types() {
        let filtered = build_schema_context_for_description("run pytest and fix errors");
        assert!(filtered.contains("### shell_command"));
        assert!(filtered.contains("### prompt"));
        assert!(filtered.contains("### check"));
        assert!(filtered.contains("### test"));
    }

    #[test]
    fn test_filtered_context_excludes_irrelevant_types() {
        let filtered = build_schema_context_for_description("run pytest and fix errors");
        assert!(!filtered.contains("### gui_action"));
        assert!(!filtered.contains("### awas_discover"));
    }

    #[test]
    fn test_context_contains_verification_rules() {
        let context = build_schema_context();
        assert!(context.contains("Verification Quality Rules"));
        assert!(context.contains("Gate step required"));
    }

    #[test]
    fn test_context_contains_ui_bridge_sdk() {
        let context = build_schema_context();
        assert!(context.contains("UI Bridge SDK"));
        assert!(context.contains("sdk_connect"));
    }

    #[test]
    fn test_context_contains_environment_aliases() {
        let context = build_schema_context();
        assert!(context.contains("Environment Aliases"));
        assert!(context.contains("localhost:3001"));
    }

    #[test]
    fn test_full_context_without_db() {
        // Should work without DB — just no examples
        let context = build_schema_context_full("run pytest", None, None);
        assert!(context.contains("### shell_command"));
        assert!(context.contains("### test"));
    }

    #[test]
    fn test_context_contains_sdk_response_schemas() {
        let context = build_schema_context();
        assert!(context.contains("SDK Response Schemas"));
        assert!(context.contains("ai/search"));
        assert!(context.contains("results"));
        assert!(context.contains("status_code: 200"));
        assert!(context.contains("body_contains"));
    }

    #[test]
    fn test_context_contains_assertion_best_practices() {
        let context = build_schema_context();
        assert!(context.contains("SDK assertions must verify content"));
        assert!(context.contains("INSUFFICIENT"));
    }

    #[test]
    fn test_context_contains_agentic_verification_rule() {
        let context = build_schema_context();
        assert!(context.contains("Every agentic step must have a corresponding"));
        assert!(context.contains("deterministic verification step"));
    }

    #[test]
    fn test_generate_step_types_documentation() {
        let all_types = get_all_step_type_metadata();
        let type_refs: Vec<&StepTypeMetadata> = all_types.iter().collect();
        let doc = generate_step_types_documentation(&type_refs);
        // Should contain field names from metadata
        assert!(doc.contains("\"command\""));
        assert!(doc.contains("\"check_type\""));
        assert!(doc.contains("\"method\""));
    }
}
