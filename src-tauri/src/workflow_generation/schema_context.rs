//! Schema Context for Workflow Generation
//!
//! Builds the AI prompt with workflow schema documentation and examples.
//! Documentation is auto-generated from the step type metadata registry
//! rather than hardcoded static strings.

use rusqlite::Connection;

use super::example_workflows::{find_relevant_examples, format_examples_for_prompt};
use super::relevance_filter::filter_relevant_step_types;
use super::rules;
use super::step_type_knowledge;
use super::step_type_metadata::{get_all_step_type_metadata, StepTypeMetadata};
use crate::skills::SkillRegistry;

/// Build the complete schema context prompt for AI workflow generation.
///
/// Backward-compatible: uses all step types and no RAG examples.
pub fn build_schema_context() -> String {
    let all_types = get_all_step_type_metadata();
    let type_refs: Vec<&StepTypeMetadata> = all_types.iter().collect();
    let step_types_doc = generate_step_types_documentation(&type_refs);
    let phase_table = generate_phase_constraint_table(&type_refs);
    assemble_prompt(&step_types_doc, &phase_table, "", "", None)
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
    assemble_prompt(&step_types_doc, &phase_table, "", "", None)
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

    // Load step type knowledge filtered to relevant step types
    let knowledge_section = if let Some(conn) = conn {
        let step_type_names: Vec<&str> = filtered.iter().map(|m| m.step_type).collect();
        let entries = step_type_knowledge::load_knowledge_for_step_types(conn, &step_type_names);
        if !entries.is_empty() {
            step_type_knowledge::format_knowledge_as_markdown(&entries)
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    assemble_prompt(
        &step_types_doc,
        &phase_table,
        &examples_section,
        &knowledge_section,
        conn,
    )
}

// ============================================================================
// Auto-Generated Documentation
// ============================================================================

/// Generate the step types documentation section from metadata.
///
/// Produces markdown blocks like:
/// ```text
/// ### command (Setup, Verification, or Completion)
/// Execute a shell command, code quality check, or test.
/// Fields:
/// - `command`: string — Shell command to execute
/// ```
pub fn generate_step_types_documentation(types: &[&StepTypeMetadata]) -> String {
    let mut output = String::with_capacity(4096);

    for meta in types {
        // Header: ### type_name (phases)
        let phases_str = format_phases_for_header(meta.allowed_phases);
        output.push_str(&format!(
            "### {} ({})\n{}\n```json\n{{\n  \"type\": \"{}\",\n  \"phase\": {},\n",
            meta.step_type,
            phases_str,
            meta.description,
            meta.step_type,
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

fn assemble_prompt(
    step_types: &str,
    phase_table: &str,
    examples: &str,
    knowledge: &str,
    conn: Option<&Connection>,
) -> String {
    let examples_section = if examples.is_empty() {
        String::new()
    } else {
        format!("## Examples\n\n{}", examples)
    };

    // Build rules from DB if available, otherwise use hardcoded fallback
    let rules_section = build_rules_section(conn);

    let knowledge_section = if knowledge.is_empty() {
        String::new()
    } else {
        knowledge.to_string()
    };

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

## Multi-Stage Workflows

For complex tasks with distinct sequential phases, use the `stages` array instead of top-level step arrays.
Each stage has its own setup, verification, agentic, and completion steps with its own verification-agentic loop.

### When to use stages
- Tasks with 2+ distinct sequential phases (e.g., research → implement → test)
- Multi-app or multi-service tasks where each target has different verification
- Tasks where each phase has genuinely different verification criteria
- Single-phase tasks should NOT use stages — use top-level steps instead
- Prefer fewer stages over more: only split when phases have distinct verification

### Stage schema
```json
{{
  "stages": [
    {{
      "id": "uuid-v4",
      "name": "Stage Name",
      "description": "What this stage does",
      "setup_steps": [...],
      "verification_steps": [...],
      "agentic_steps": [...],
      "completion_steps": [],
      "max_iterations": 5,
      "provider": "claude_cli",
      "model": "claude-sonnet-4"
    }}
  ],
  "stop_on_failure": false
}}
```

### Rules for stages
- Every stage MUST have at least one deterministic verification step
- Stages execute sequentially: Stage 1 must complete before Stage 2 starts
- Later stages see full output from all prior stages in AI context
- When using `stages`, leave top-level step arrays empty (setup_steps: [], etc.)
- `stop_on_failure` defaults to false — workflow continues even if a stage fails
- Per-stage `provider`/`model` override the workflow-level defaults

### Difficulty-based model selection per stage
- Simple stages (file checks, status checks, log reading): faster model (claude-sonnet-4, gemini-2.0-flash)
- Complex stages (implementation, debugging, refactoring): capable model (claude-opus-4, gemini-2.5-pro)
- Research/analysis stages: mid-tier model (claude-sonnet-4)

## Step Types

### Common Fields (all steps)
- `id`: UUID v4 string (required)
- `name`: Display name (required)
- `phase`: Which phase this step belongs to (required)
- `required`: boolean (optional, default true) — If false, step failure does not fail the workflow
- `depends_on`: string[] (optional) — Step IDs that must complete before this step runs. Creates a DAG execution order.
- `inputs`: object (optional) — Map of variable names to values. Values can reference other step outputs using `${{step_id.field}}` syntax.
- `extract`: object (optional) — Map of variable names to extraction expressions. Extracts values from step output for use by downstream steps via `inputs`.

### Data Flow Between Steps

Steps can pass data to each other using `inputs` and `extract`:
1. A step uses `extract` to pull values from its output (e.g., `{{"token": "$.access_token"}}`)
2. A downstream step uses `inputs` to receive those values (e.g., `{{"auth_token": "${{login_step.token}}"}}`)
3. Use `depends_on` to ensure execution order when steps need data from earlier steps
4. Circular dependencies in `depends_on` are invalid and will be rejected

{step_types}

{knowledge_section}

## Phase Constraints

{phase_table}

{examples_section}

{rules_section}

## Environment Aliases
When the user mentions these terms, map them to the correct endpoints:
- "runner" / "desktop app" → Qontinui Runner at http://localhost:1420 (UI) / http://localhost:9876 (API)
- "web" / "web app" / "frontend" → Qontinui Web at http://localhost:3001
- "backend" / "API" / "server" → Web backend at http://localhost:8000
- "supervisor" / "dev supervisor" → Qontinui Supervisor at http://localhost:9875 (health: GET /health, NOT /status)
- Use health check API requests to verify services are running before testing. The supervisor uses GET /health (NOT /status).

**IMPORTANT: Runner API Response Format**
ALL runner API endpoints (port 9876) return responses in the standard `ApiResponse` format:
```json
{{"success": true, "data": <payload>}}    // on success
{{"success": false, "error": "<message>"}} // on error
```
- Health check: `GET http://localhost:9876/health` returns `{{"success": true, "data": "ok"}}`
- To verify health: `curl -s http://localhost:9876/health | python -c "import sys,json; d=json.load(sys.stdin); assert d.get('success')==True and d.get('data')=='ok', 'Runner not healthy: '+str(d)"`
- NEVER check for `d.get('status')` — the field is `d.get('data')` for the payload and `d.get('success')` for success/failure.

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
  "type": "command",
  "phase": "setup",
  "name": "Connect UI Bridge SDK",
  "command": "curl -s -X POST http://localhost:9876/ui-bridge/sdk/connect -H \"Content-Type: application/json\" -d \"{{\\\"url\\\": \\\"http://localhost:3001\\\"}}\"",
  "fail_on_error": true
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
{{"data": [{{"id": "...", "type": "button", "tagName": "BUTTON", "state": {{"textContent": "Save"}}}}], "success": true, "timestamp": 123456}}
```
- Elements are in the `data` array (NOT `elements` or `total` fields)
- Good assertion: `body_contains` with a known element ID or text content, or pipe to `python -c "import sys,json; d=json.load(sys.stdin); assert len(d.get('data',[]))>0"`

**`GET /ui-bridge/sdk/snapshot`** — Returns full page state:
```json
{{"data": {{"elements": [...], "components": [...], "timestamp": 123456, "workflows": [...]}}, "success": true, "timestamp": 123456}}
```
- Elements are nested under `data.elements`
- Good assertion: `body_contains` with expected page title or element content

**`POST /ui-bridge/sdk/discover`** — Returns discovered elements:
```json
{{"data": {{"elements": [...]}}, "success": true, "timestamp": 123456}}
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

### Retry Support for Verification Commands
After page navigation, the WebSocket connection needs ~15 seconds to reconnect. Use `retry_count` and `retry_delay_ms` on verification steps that follow navigation:
```json
{{
  "type": "command",
  "phase": "verification",
  "name": "Verify page loaded",
  "command": "curl -s http://localhost:9876/ui-bridge/sdk/snapshot | python -c \"import sys,json; d=json.load(sys.stdin); assert d.get('success'), 'Snapshot failed: '+str(d.get('error','unknown'))\"",
  "retry_count": 5,
  "retry_delay_ms": 3000
}}
```
This retries up to 5 times with 3s delay between attempts — enough time for the WebSocket to reconnect after navigation.

**IMPORTANT:** Never use `curl -sf` (with `-f` flag) when piping output to another command like `python -c` or `grep`. The `-f` flag makes curl suppress ALL output on HTTP errors, so the piped command receives empty input and fails with a confusing error. Use `curl -s` instead and validate the response in the piped command.

### SDK in Verification Steps
Use `command` steps with curl to query SDK endpoints for verification:
```json
{{
  "type": "command",
  "phase": "verification",
  "name": "Check Page Content",
  "command": "curl -s http://localhost:9876/ui-bridge/sdk/elements?contentOnly=true | python -c \"import sys,json; d=json.load(sys.stdin); assert d.get('success'), 'Request failed: '+str(d.get('error','unknown')); assert len(d.get('data',[]))>0, 'No elements found'\""
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
// Skill Catalog for Generator
// ============================================================================

/// Format the built-in skill catalog as a compact markdown section for the
/// AI generator prompt. This gives the AI awareness of pre-configured skill
/// templates that it can reference in its generated workflows.
///
/// The output is a compact catalog grouped by category (~500 tokens) that
/// tells the AI what skills are available and their parameters.
pub fn format_skills_for_generator(registry: &SkillRegistry) -> String {
    let skills = registry.all();
    if skills.is_empty() {
        return String::new();
    }

    let mut output = String::with_capacity(2048);
    output.push_str("## Available Skills (pre-configured step templates)\n\n");
    output
        .push_str("Skills are reusable step templates available in the UI. When your generated\n");
    output.push_str("workflow needs common operations (linting, testing, health checks), prefer\n");
    output.push_str("using the equivalent step configuration that matches these skills.\n\n");

    // Group by category
    let mut categories: std::collections::BTreeMap<&str, Vec<&crate::skills::SkillDefinition>> =
        std::collections::BTreeMap::new();
    for skill in &skills {
        categories.entry(&skill.category).or_default().push(skill);
    }

    // Category display names
    let category_labels: std::collections::HashMap<&str, &str> = [
        ("code-quality", "Code Quality"),
        ("testing", "Testing"),
        ("monitoring", "Monitoring"),
        ("ai-task", "AI Task"),
        ("deployment", "Deployment"),
        ("composition", "Composition"),
        ("custom", "Custom"),
    ]
    .into_iter()
    .collect();

    for (category, cat_skills) in &categories {
        let label = category_labels.get(category).unwrap_or(category);
        output.push_str(&format!("### {}\n", label));

        for skill in cat_skills {
            // Build compact param list
            let params: Vec<String> = skill
                .parameters
                .iter()
                .map(|p| {
                    if p.required {
                        p.name.clone()
                    } else {
                        format!("{}?", p.name)
                    }
                })
                .collect();
            let params_str = if params.is_empty() {
                String::from("(no params)")
            } else {
                params.join(", ")
            };

            // Build compact phase list
            let phases: Vec<&str> = skill
                .allowed_phases
                .iter()
                .map(|p| match p.as_str() {
                    "setup" => "S",
                    "verification" => "V",
                    "agentic" => "A",
                    "completion" => "C",
                    _ => p.as_str(),
                })
                .collect();
            let phases_str = phases.join("/");

            output.push_str(&format!(
                "- **{}**: {}. Params: {}. Phase: {}.\n",
                skill.slug, skill.description, params_str, phases_str,
            ));
        }
        output.push('\n');
    }

    output
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

6. **Deterministic verification step required**: verification_steps MUST include at least one deterministic, automated step — one of: `command` (with check_type or a command), `test`, or `ui_bridge` (assert action). Do NOT use only `prompt` type steps for verification.
7. **Code modification requires typecheck**: When the workflow creates or modifies source code files, verification MUST include a `command` step with `check_type` set to the appropriate type checker.
8. **Web app verification requires SDK or Playwright**: When the workflow targets a web application, verification MUST include `ui_bridge` steps or Playwright-based checks.
9. **Data flow references must be valid**: All step IDs referenced in `inputs`, `depends_on`, and `extract` must correspond to existing step IDs within the same workflow. Circular dependencies in `depends_on` are invalid.
10. **Prompts are supplementary**: `prompt` type steps in verification must NEVER be the sole verification mechanism.
11. **Test steps with inline commands use repository**: When a `test` step runs a shell command, set `test_type: "repository"`.
12. **Next.js App Router path conventions**: Use correct paths like `src/app/(app)/`.
13. **Verification step caching**: Running tasks cache verification steps at startup; API updates don't affect current task.
14. **Feature-specific verification required**: Must include feature-specific checks, not just compilation/lint checks.
15. **Agentic-verification correspondence**: Each agentic step must have a corresponding deterministic verification step.
16. **Only 3 step types exist**: The only valid step types are `command`, `ui_bridge`, and `prompt`. Do NOT use `shell_command`, `api_request`, `mcp_call`, `check`, `check_group`, `test`, `gate`, or `spec` — these are not valid types. Tests are run via `command` with `test_type` set. Checks are run via `command` with `check_type` set.
17. **Every command step MUST include a mode field**: Valid modes are `shell`, `check`, `check_group`, `test`. The mode must match the fields present: `check` requires `check_type`, `check_group` requires `check_group_id`, `test` requires `test_type` or `test_id`, `shell` is for plain commands.
18. **No Python f-strings or jq in shell commands**: When piping curl output to `python -c`, NEVER use f-strings (`f'...'`). Use string concatenation instead: `'Expected >2, got ' + str(len(elems))`. F-strings with nested quotes create unresolvable JSON escaping issues. Do NOT use `jq` — it is not available on Windows. For JSON assertions, pipe to `python -c \"import sys,json; d=json.load(sys.stdin); assert len(d.get('elements',[]))>0\"`. For checking fields exist, use `grep` (e.g., `curl ... | grep 'elements'`).
19. **Retry format**: Use flat `retry_count` and `retry_delay_ms` fields directly on step objects. Do NOT use a nested `retry` object (e.g., `\"retry\": {\"count\": 5}` is WRONG, use `\"retry_count\": 5`).
20. **No bash negation prefix in shell commands**: NEVER use `!` as a command prefix to invert exit codes (e.g., `! grep -qE 'pattern' file`). The `!` operator is bash-specific and may not be detected by the shell router on Windows, causing false negatives. Instead, use explicit exit code handling: `grep -qE 'pattern' file && exit 1 || exit 0` to assert something is NOT present.
21. **Removal tasks require runtime verification**: When a workflow involves removing pages/routes/components from a web app, verification MUST include runtime checks (UI Bridge navigate + assertion, or HTTP request to the removed URL), not just static file-existence checks (`test ! -f`). Build caches and routing config may still serve removed content after source file deletion.
22. **Per-stage UI verification in multi-stage workflows**: In multi-stage workflows, each stage that modifies UI must have its own UI Bridge verification steps. A UI Bridge check in one stage does NOT cover UI changes made in another stage."#;

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
        assert!(context.contains("### command"));
        assert!(context.contains("### prompt"));
        assert!(context.contains("### ui_bridge"));
        // "test" was merged into "command" — no longer a separate heading
        assert!(!context.contains("### test"));
    }

    #[test]
    fn test_build_schema_context_contains_phase_table() {
        let context = build_schema_context();
        assert!(context.contains("| Step Type | setup | verification | agentic | completion |"));
        assert!(context.contains("| command |"));
    }

    #[test]
    fn test_filtered_context_smaller_than_full() {
        let full = build_schema_context();
        let filtered = build_schema_context_for_description("run pytest and fix errors");
        // Filtered should be smaller (no automation types)
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
        assert!(filtered.contains("### command"));
        assert!(filtered.contains("### prompt"));
    }

    #[test]
    fn test_filtered_context_excludes_irrelevant_types() {
        let filtered = build_schema_context_for_description("run pytest and fix errors");
        assert!(!filtered.contains("### ui_bridge"));
    }

    #[test]
    fn test_context_contains_verification_rules() {
        let context = build_schema_context();
        assert!(context.contains("Verification Quality Rules"));
        assert!(context.contains("Data flow references must be valid"));
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
        assert!(context.contains("### command"));
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
    fn test_context_contains_deterministic_verification_rule() {
        let context = build_schema_context();
        assert!(context.contains("Deterministic verification step required"));
    }

    #[test]
    fn test_context_contains_agentic_verification_rule() {
        let context = build_schema_context();
        assert!(context.contains("Each agentic step must have a corresponding"));
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
        assert!(doc.contains("\"action\""));
    }

    #[test]
    fn test_format_skills_for_generator() {
        let registry = SkillRegistry::new();
        let output = format_skills_for_generator(&registry);

        // Should have the header
        assert!(output.contains("## Available Skills"));

        // Should contain category headers
        assert!(output.contains("### Code Quality"));
        assert!(output.contains("### Testing"));
        assert!(output.contains("### Monitoring"));

        // Should contain skill slugs
        assert!(output.contains("**lint-project**"));
        assert!(output.contains("**run-tests**"));
        assert!(output.contains("**api-health-check**"));

        // Should contain parameter names
        assert!(output.contains("working_directory"));
        assert!(output.contains("test_type"));

        // Should contain phase abbreviations
        assert!(output.contains("S/V")); // setup/verification
    }

    #[test]
    fn test_format_skills_for_generator_compact() {
        let registry = SkillRegistry::new();
        let output = format_skills_for_generator(&registry);

        // Should be reasonably compact (under 3000 chars for 15 skills)
        assert!(
            output.len() < 3000,
            "Skill catalog too verbose: {} chars",
            output.len()
        );
        // But not empty
        assert!(output.len() > 500);
    }

    #[test]
    fn test_format_skills_empty_registry() {
        // An empty registry (no builtins loaded) would return empty string
        // We can't easily test this without a custom registry, but we can
        // verify the function handles the data correctly
        let registry = SkillRegistry::new();
        let output = format_skills_for_generator(&registry);
        assert!(!output.is_empty());
    }
}
