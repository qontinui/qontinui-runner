/**
 * spec-prompt-builder.ts
 *
 * Builds a structured AI prompt from discovered UI Bridge spec data.
 * Used by AiGeneratePanel to create spec-driven workflow generation requests.
 */

// =============================================================================
// Types (mirrors the spec JSON structure)
// =============================================================================

export interface SpecAssertion {
  id: string;
  description: string;
  category: string;
  severity: "critical" | "warning" | "info";
  enabled: boolean;
  target?: Record<string, unknown>;
  assertionType?: string;
  expected?: unknown;
  condition?: Record<string, unknown>;
  precondition?: string;
}

export interface SpecGroup {
  id: string;
  name: string;
  description: string;
  category: string;
  assertions: SpecAssertion[];
  source?: string;
}

export interface SpecConfig {
  version: string;
  description?: string;
  groups: SpecGroup[];
  metadata?: {
    component?: string;
    pageUrl?: string;
    tags?: string[];
    [key: string]: unknown;
  };
}

export interface DiscoveredSpec {
  specId: string;
  config: SpecConfig;
  /** The application this spec belongs to (e.g. "Qontinui Web", "Qontinui Runner") */
  appName?: string;
}

// =============================================================================
// Prompt builder
// =============================================================================

export interface BuildSpecPromptOptions {
  discoveredSpecs: DiscoveredSpec[];
  selectedGroupIds: Set<string>;
}

export interface SpecPromptResult {
  prompt: string;
  totalGroups: number;
  totalAssertions: number;
  pageCount: number;
}

/**
 * Format a SpecTarget into a human-readable search description.
 */
function formatTarget(target?: Record<string, unknown>): string {
  if (!target) return "";
  const type = target.type as string | undefined;
  if (type === "search") {
    const criteria = target.criteria as Record<string, unknown> | undefined;
    if (!criteria) return "";
    const parts: string[] = [];
    if (criteria.role) parts.push(`role=${criteria.role}`);
    if (criteria.textContent) parts.push(`text="${criteria.textContent}"`);
    if (criteria.idPattern) parts.push(`id~${criteria.idPattern}`);
    if (criteria.tagName) parts.push(`tag=${criteria.tagName}`);
    return parts.length > 0 ? `(search: ${parts.join(", ")})` : "";
  }
  if (type === "elementId") {
    return `(elementId: ${target.elementId})`;
  }
  return "";
}

/**
 * Format a condition into a human-readable string.
 */
function formatCondition(condition?: Record<string, unknown>): string {
  if (!condition) return "";
  const type = condition.type as string;
  const condTarget = condition.target as Record<string, unknown> | undefined;
  const targetStr = formatTarget(condTarget);
  if (type === "exists") return `[when ${targetStr} exists]`;
  if (type === "notExists") return `[when ${targetStr} absent]`;
  if (type === "hasText") return `[when ${targetStr} has text "${condition.text}"]`;
  return "";
}

/**
 * Build a comprehensive prompt from all selected spec groups.
 * Includes assertion types, targets, conditions, and severity levels
 * to guide the AI in generating diverse verification steps.
 */
export function buildSpecPrompt({
  discoveredSpecs,
  selectedGroupIds,
}: BuildSpecPromptOptions): SpecPromptResult {
  const lines: string[] = [];

  // Assertion type vocabulary
  lines.push("# UI Bridge Spec Assertion Types");
  lines.push("");
  lines.push("The following assertion types are available for verification steps:");
  lines.push("- **exists** / **notExists** — element presence or absence");
  lines.push("- **visible** / **hidden** — element visibility state");
  lines.push("- **enabled** / **disabled** — interactive element enabled/disabled state");
  lines.push("- **focused** — element has keyboard focus");
  lines.push("- **checked** / **unchecked** — checkbox/radio state");
  lines.push("- **hasText** / **containsText** — exact or partial text content match");
  lines.push("- **hasValue** — form input value match");
  lines.push("- **count** — number of matching elements (expected = number)");
  lines.push("- **attribute** — element attribute value (requires attributeName + expected)");
  lines.push("- **hasClass** — element has a CSS class");
  lines.push("- **cssProperty** — computed CSS property value");
  lines.push("");
  lines.push(
    "Severity levels: **critical** (core functionality), **warning** (important features), **info** (nice-to-have).",
  );
  lines.push("");
  lines.push("---");
  lines.push("");
  lines.push("# Page Specifications");
  lines.push("");

  let totalGroups = 0;
  let totalAssertions = 0;
  const pageUrls = new Set<string>();

  for (const spec of discoveredSpecs) {
    const { config } = spec;
    const pageUrl = config.metadata?.pageUrl || spec.specId;

    const selectedGroups = config.groups.filter((g) => selectedGroupIds.has(g.id));
    if (selectedGroups.length === 0) continue;

    pageUrls.add(pageUrl);
    lines.push(`## ${pageUrl}`);
    lines.push("");

    for (const group of selectedGroups) {
      totalGroups++;
      lines.push(`### ${group.name} [${group.category}]`);
      if (group.description) {
        lines.push(group.description);
      }
      lines.push("");

      const enabledAssertions = group.assertions.filter((a) => a.enabled);
      if (enabledAssertions.length > 0) {
        lines.push("Assertions:");
        for (const assertion of enabledAssertions) {
          totalAssertions++;
          const severity = assertion.severity || "info";
          const type = assertion.assertionType || "exists";
          const targetStr = formatTarget(assertion.target);
          const condStr = formatCondition(assertion.condition);
          const expectedStr =
            assertion.expected !== undefined && assertion.expected !== null
              ? ` expected=${JSON.stringify(assertion.expected)}`
              : "";
          const precondStr = assertion.precondition
            ? ` | precondition: "${assertion.precondition}"`
            : "";

          lines.push(
            `- [${severity}] ${assertion.description} — **${type}**${expectedStr} ${targetStr} ${condStr}${precondStr}`.trimEnd(),
          );
        }
        lines.push("");
      }
    }
  }

  const pageCount = pageUrls.size;

  // Closing guidance
  lines.push("---");
  lines.push("");
  lines.push("## Verification Step Guidance");
  lines.push("");
  lines.push("Use the page specifications above to generate appropriate verification steps:");
  lines.push("- Do NOT use only `exists` assertions — verify state, content, and behavior.");
  lines.push("- Use `enabled`/`disabled` to verify interactive element states.");
  lines.push("- Use `hasText`/`containsText` to verify text content and labels.");
  lines.push("- Use `count` to verify the expected number of repeated elements.");
  lines.push("- Use `hasValue` to verify form input values.");
  lines.push("- Use conditions when assertions only apply in certain UI states.");
  lines.push(
    "- Match severity levels to importance: critical for core flows, warning for features, info for polish.",
  );

  return {
    prompt: lines.join("\n"),
    totalGroups,
    totalAssertions,
    pageCount,
  };
}

/** @deprecated Use `buildSpecPrompt` instead. */
export const buildSemanticSpecPrompt = buildSpecPrompt;

// =============================================================================
// Spec creation & review instructions
// =============================================================================

/**
 * Comprehensive instructions for AI-driven spec creation.
 * Teaches the AI the full JSON schema, quality standards, and best practices.
 */
export const SPEC_CREATION_INSTRUCTIONS = `
# UI Bridge Spec Creation Guide

You are creating specifications for UI pages in the UI Bridge spec format. These specs serve two critical purposes:
1. **Automated verification** — workflows use them to verify page correctness
2. **Bug fixing** — AI workflows read specs to understand expected behavior and fix issues autonomously

## CRITICAL: Read the Source Code First

**You MUST explore the actual source code before writing any spec.** Do not guess at page structure or behavior — read the components, hooks, API calls, and data models to understand what the page actually does.

### Code exploration steps (do ALL of these):
1. **Find the page component** — search for the page route or component file (e.g., \`find src -name "*page*" -path "*/runs/*"\`)
2. **Read the page component** — understand what child components it renders, what tabs/sections it has
3. **Read child components** — each widget, panel, form, or section. Note their props, state, conditional rendering, empty states, error handling
4. **Read hooks and state** — find \`use*\` hooks the page uses. Understand what data is fetched, what actions are available, what state transitions exist
5. **Read API calls** — find fetch/axios calls or API route handlers. Understand what data the page expects and what operations it supports
6. **Check data attributes** — look for \`data-content-label\`, \`data-testid\`, \`aria-label\`, \`role\` attributes in the JSX — use these in target criteria
7. **Understand the data model** — what entities does this page display? What are their fields and relationships?

### Follow the code across projects
Pages often trigger operations in other packages or libraries. You MUST follow these dependencies to understand expected results:
- If a page calls a **backend API** (e.g., FastAPI in \`qontinui-web/backend/\`), read the API route handler and the service it calls to understand what data is returned, what validations exist, and what side effects occur
- If a page triggers operations in the **qontinui core library** (e.g., state machine creation, image recognition), read the library code to understand what the operation produces — its inputs, outputs, data structures, and success/failure modes
- If a page uses a **shared package** (e.g., \`@qontinui/workflow-utils\`, \`@qontinui/shared-types\`), read the type definitions and utility functions to understand the data model
- If a page interacts with the **Rust backend** via Tauri IPC, read the Rust command handler to understand the full operation

The goal is to write semantic assertions that describe expected results with the same precision as someone who has read the code. For example, if a "Discover States" button triggers state discovery in the qontinui library, the spec should describe what states are expected to be discovered, what format they take, and how they appear in the UI — not just "states are discovered."

### What to extract from code for specs:
- **Rendered text** — headings, button labels, tab names, placeholder text, empty state messages, error messages
- **Conditional rendering** — what shows/hides based on state? What are the empty/loading/error states?
- **User interactions** — what can the user click, type, select, drag? What happens when they do?
- **Data flow** — what data is fetched, how is it displayed, what transformations happen?
- **Business logic** — validation rules, required fields, state machine transitions, creation flows
- **Cross-project operations** — what happens in the backend/library when the user triggers an action? What are the expected results?

## JSON Schema

A spec file has this top-level structure:
\`\`\`json
{
  "version": "1.0.0",
  "description": "Comprehensive description of the page and its purpose",
  "groups": [ /* array of SpecGroup */ ],
  "metadata": {
    "component": "ComponentName",
    "pageUrl": "/path/to/page",
    "tags": ["tag1", "tag2"]
  }
}
\`\`\`

### SpecGroup
\`\`\`json
{
  "id": "unique-group-id",
  "name": "Human-Readable Group Name",
  "description": "What this group tests and why it matters",
  "category": "element-presence | navigation | interaction | data-display | behavior | semantic",
  "assertions": [ /* array of SpecAssertion */ ],
  "source": "manual"
}
\`\`\`

### SpecAssertion
\`\`\`json
{
  "id": "unique-assertion-id",
  "description": "Clear description of what is being verified",
  "category": "element-presence | navigation | interaction | data-display | behavior | semantic",
  "severity": "critical | warning | info",
  "target": {
    "type": "search",
    "criteria": {
      "role": "button | heading | tab | link | textbox | ...",
      "textContent": "Exact or partial text to find",
      "idPattern": "regex-pattern-for-element-id",
      "tagName": "div | span | button | ...",
      "dataContentLabel": "data-content-label attribute value"
    },
    "label": "Human-readable label for this target"
  },
  "assertionType": "exists | notExists | visible | hidden | enabled | disabled | hasText | containsText | count | hasValue | attribute | hasClass | behavior | semantic",
  "expected": "expected value (for count, hasText, attribute, etc.)",
  "precondition": "Description of required page state before this assertion applies",
  "source": "manual",
  "reviewed": true,
  "enabled": true
}
\`\`\`

## Assertion Types — Choosing the Right Type

**This is critical.** The assertionType determines whether a workflow step runs as a fast deterministic check or requires AI evaluation. Using the wrong type produces broken workflows.

### Deterministic types (fast, no AI needed)
Use these ONLY for assertions that can be verified by inspecting a single UI snapshot — checking whether an element exists, what text it shows, or what state it's in:
- **exists** — "A button labeled 'Save' is present on the page"
- **notExists** — "No error banner is shown on successful load"
- **visible** / **hidden** — "The settings panel is visible when the Settings tab is selected"
- **enabled** / **disabled** — "The Submit button is disabled until the form is valid"
- **hasText** / **containsText** — "The heading shows 'Active Runs'"
- **count** — "There are exactly 6 tabs" (set \`expected\` to a number)
- **hasValue** — "The search input contains the user's query"
- **attribute** — "The button has aria-pressed='true'"
- **hasClass** — "The selected tab has the 'active' CSS class"

### Semantic types (require AI evaluation)
Use these for ANYTHING that describes interaction results, multi-step flows, data relationships, or outcomes that can't be verified from a static snapshot:
- **behavior** — user interaction produces expected results. Use when the description mentions clicking, submitting, creating, deleting, navigating, or any action that changes state.
- **semantic** — data model correctness, algorithm outputs, content meaning, cross-system relationships. Use when the description discusses data structures, expected API responses, or how systems connect.

### Decision rule
Ask: "Can this be verified by looking at a single UI snapshot without performing any action?"
- **Yes** → use a deterministic type (\`exists\`, \`visible\`, \`hasText\`, etc.)
- **No** → use \`behavior\` (for interactions) or \`semantic\` (for data/logic)

### Common mistakes — DO NOT do these:
- **WRONG:** \`assertionType: "exists"\` with description "Clicking Save should persist the data and show a success toast" — this is a behavior, not an existence check
- **WRONG:** \`assertionType: "exists"\` with description "The discovery workflow creates states grouped by element co-occurrence" — this is semantic, not an existence check
- **CORRECT:** \`assertionType: "exists"\` with description "A 'Save' button is present in the toolbar"
- **CORRECT:** \`assertionType: "behavior"\` with description "Clicking Save should persist the data and show a success toast"
- **CORRECT:** \`assertionType: "semantic"\` with description "The discovery workflow creates states grouped by element co-occurrence"

## Quality Standards

### Group Organization
- **10-20 groups** per spec for a complex page, 5-10 for simple pages
- Each group focuses on ONE aspect: structure, navigation, a specific widget, a workflow, etc.
- Group descriptions should explain the purpose and context, not just list what's tested
- Use descriptive IDs like \`page-tab-navigation\`, \`widget-timeline\`, \`creation-flow\`

### Assertion Quality
- **3-6 assertions per group** — enough to be thorough without being noisy
- Every group should have at least one \`critical\` severity assertion
- Use \`precondition\` when assertions depend on page state (e.g., "A run must be active", "Discovery tab must be selected")
- Write descriptions as clear, testable statements
- Target criteria should use the most stable identifiers: \`role\`, \`data-content-label\`, \`textContent\`
- Use actual text strings, data attributes, and ARIA roles you found in the source code — not guesses

### Writing behavior and semantic assertions
For \`assertionType: "behavior"\` and \`assertionType: "semantic"\`, write descriptions detailed enough for an AI to:
1. Understand the expected outcome
2. Know what "correct" looks like
3. Fix the feature if it's broken

These assertions MUST use \`assertionType: "behavior"\` or \`assertionType: "semantic"\` — NEVER \`"exists"\`.

Ground them in the actual code behavior:

**Bad:** assertionType "exists" + description "State creation works" — wrong type AND vague
**Good:** assertionType **"behavior"** + description "Creating a new state machine config via the 'New Config' button should open a creation dialog, accept a name, and after saving, the new config should appear in the config selector dropdown and be automatically selected. The API call POST /api/state-machines should return the new config with an auto-generated ID."

**Bad:** assertionType "exists" + description "Form validates input" — wrong type AND vague
**Good:** assertionType **"behavior"** + description "The workflow name field requires at least 1 character. Submitting with an empty name should show an inline error message 'Name is required' below the input. The Save button should remain enabled but the form should not submit until the error is resolved."

### Severity Guidelines
- **critical** — core page purpose, primary navigation, data integrity. Page is broken without this.
- **warning** — important features, secondary navigation, user experience. Page works but is degraded.
- **info** — nice-to-have, polish, cosmetic. Low impact if missing.

### Preconditions
Use preconditions liberally. They tell the automation system what state the page needs to be in:
- "Page has loaded with at least one item in the list"
- "The user has navigated to the Settings tab"
- "A workflow is currently running"
- "The creation dialog is open"

## Example Group (showing all three assertion types)

\`\`\`json
{
  "id": "settings-form",
  "name": "Settings Form",
  "description": "The Settings tab contains a form for editing configuration values, with save and reset actions.",
  "category": "interaction",
  "assertions": [
    {
      "id": "settings-save-button-exists",
      "description": "A 'Save' button is present in the settings form",
      "category": "element-presence",
      "severity": "critical",
      "target": {
        "type": "search",
        "criteria": { "role": "button", "textContent": "Save" },
        "label": "Save button"
      },
      "assertionType": "exists",
      "source": "manual",
      "reviewed": true,
      "enabled": true
    },
    {
      "id": "settings-save-persists",
      "description": "Clicking Save submits the form via PUT /api/settings, shows a success toast 'Settings saved', and the form retains the new values. If the API returns a validation error, the form shows inline error messages under the invalid fields.",
      "category": "behavior",
      "severity": "critical",
      "target": {
        "type": "search",
        "criteria": { "role": "button", "textContent": "Save" },
        "label": "Save button"
      },
      "assertionType": "behavior",
      "precondition": "Settings tab is selected and at least one field has been modified",
      "source": "manual",
      "reviewed": true,
      "enabled": true
    },
    {
      "id": "settings-data-model",
      "description": "Settings are stored as key-value pairs where keys follow the pattern 'section.property' (e.g., 'ai.model', 'runner.timeout'). The API returns all settings as a flat JSON object. Boolean settings render as toggle switches, string settings as text inputs, and numeric settings as number inputs with min/max constraints from the schema.",
      "category": "semantic",
      "severity": "warning",
      "target": {
        "type": "search",
        "criteria": { "role": "form" },
        "label": "Settings form"
      },
      "assertionType": "semantic",
      "source": "manual",
      "reviewed": true,
      "enabled": true
    }
  ],
  "source": "manual"
}
\`\`\`

Notice: the Save button gets TWO assertions — \`exists\` (is it there?) and \`behavior\` (what happens when you click it?). This is the correct pattern. Do NOT put behavioral descriptions on \`exists\` assertions.

## Instructions

1. **Read the source code first** — explore components, hooks, API calls, and data models before writing any assertions
2. Always output a complete, valid JSON spec (not partial)
3. **Choose assertionType based on what is being verified, not the element being targeted.** If the description mentions an interaction result, state change, or multi-step flow, it MUST be \`behavior\` or \`semantic\`, never \`exists\`. Review every assertion before finalizing: does the description match the type?
4. Every group should contain a mix of types — typically 2-3 deterministic (\`exists\`, \`visible\`, \`hasText\`) plus 1-2 semantic (\`behavior\`, \`semantic\`). A spec with only \`exists\` assertions is incomplete.
5. Cover the full page lifecycle: loading, empty states, populated states, interactions, error states
6. Use consistent ID prefixes per page (e.g., "sm-" for state machine, "ar-" for active runs)
7. Include metadata with component name, page URL, and relevant tags
8. Write the top-level description as a comprehensive summary of what the page does
9. Ground all assertions in what you actually found in the code — use real text content, real data attributes, real component names
`.trim();

/**
 * Build detailed spec context for AI review — includes full assertion details.
 * Unlike the summary version, this gives the AI everything it needs to
 * meaningfully review, critique, and improve specs.
 */
export function buildDetailedSpecContext(spec: {
  specId: string;
  config: SpecConfig;
  appName?: string;
}): string {
  const { config } = spec;
  const lines: string[] = [];

  lines.push(`# Spec: ${spec.specId}`);
  if (spec.appName) lines.push(`Application: ${spec.appName}`);
  if (config.description) lines.push(`Description: ${config.description}`);
  if (config.metadata?.pageUrl) lines.push(`Page URL: ${config.metadata.pageUrl}`);
  if (config.metadata?.tags?.length) lines.push(`Tags: ${config.metadata.tags.join(", ")}`);
  lines.push(`Version: ${config.version}`);
  lines.push("");

  const groups = config.groups || [];
  lines.push(`## Groups (${groups.length} total)`);
  lines.push("");

  for (const group of groups) {
    const enabledCount = group.assertions.filter((a) => a.enabled).length;
    const disabledCount = group.assertions.length - enabledCount;

    lines.push(`### ${group.name} [${group.category}]`);
    lines.push(`ID: ${group.id}`);
    if (group.description) lines.push(group.description);
    lines.push(
      `Assertions: ${enabledCount} enabled${disabledCount > 0 ? `, ${disabledCount} disabled` : ""}`,
    );
    lines.push("");

    for (const assertion of group.assertions) {
      const severity = assertion.severity || "info";
      const type = assertion.assertionType || "exists";
      const status = assertion.enabled ? "" : " [DISABLED]";
      const targetStr = formatTarget(assertion.target);
      const expectedStr =
        assertion.expected !== undefined && assertion.expected !== null
          ? ` | expected: ${JSON.stringify(assertion.expected)}`
          : "";
      const precondStr = assertion.precondition
        ? `\n    Precondition: ${assertion.precondition}`
        : "";

      lines.push(`- [${severity}]${status} **${assertion.id}**: ${assertion.description}`);
      lines.push(`    Type: ${type} ${targetStr}${expectedStr}${precondStr}`);
    }
    lines.push("");
  }

  return lines.join("\n");
}

/**
 * Build review instructions with the full spec content.
 */
export function buildSpecReviewPrompt(
  spec: { specId: string; config: SpecConfig; appName?: string },
  reviewType: "quality" | "gaps" | "improvements" | "assertions",
): string {
  const specContext = buildDetailedSpecContext(spec);

  const codeExplorationPreamble = `**IMPORTANT: Before responding, you MUST read the source code for this page.** Use the page URL, component name, and spec description to find the relevant files. Read:
1. The page component and its child components — understand what's rendered, what conditional states exist
2. Hooks and state management — understand data fetching, user actions, state transitions
3. API routes or backend services the page calls — understand what data is expected
4. Related libraries or utilities — if the page triggers operations in other packages (e.g., a core library), read that code to understand what the expected results should look like

Ground your analysis in the actual code, not guesses.\n\n`;

  const reviewInstructions: Record<string, string> = {
    quality: `${codeExplorationPreamble}Review this spec for quality by comparing it against the actual source code. Evaluate:
- Are assertion descriptions accurate to what the code actually renders and does?
- Do target criteria use real text, data attributes, and ARIA roles from the source code?
- Are there assertions describing behavior that doesn't match the actual code logic?
- Are semantic assertions detailed enough to describe what the code actually does (not vague summaries)?
- Are severity levels appropriate (critical for core functionality, warning for features, info for polish)?
- Are preconditions specified where assertions depend on page state?
- Is the mix of deterministic and semantic assertions balanced?

Rate each group on a scale of 1-5 and suggest specific improvements with code references.`,

    gaps: `${codeExplorationPreamble}Identify gaps in this specification by reading the source code and finding functionality not covered by the spec. Look for:
- Components or widgets rendered by the page that have no assertions
- Conditional rendering paths (empty states, loading states, error states) not covered
- User interactions (buttons, forms, dialogs) with no behavioral assertions
- Data fetching and display logic not verified
- State transitions and side effects not described
- Operations that call into other libraries/packages — the spec should describe the expected results
- Edge cases visible in the code (error handling, boundary conditions, fallbacks)

For each gap found, cite the source file and line, then suggest a specific assertion with id, description, severity, target, and assertionType.`,

    improvements: `${codeExplorationPreamble}Suggest specific improvements to make this spec more comprehensive and maintainable. Read the source code and compare:
- Assertions that are vague or don't match what the code actually does — rewrite with specific behavior from the code
- Semantic assertions that are too shallow — enrich them with the actual data flow, API calls, and state transitions from the code
- Target criteria that use fragile selectors — find data-content-label, role, or other stable attributes from the JSX
- Missing preconditions that would prevent false failures
- Groups that should be split or merged based on actual component boundaries

Provide concrete before/after examples for each improvement, citing the source code.`,

    assertions: `${codeExplorationPreamble}Generate additional assertions to strengthen this spec's coverage. Read the source code to find uncovered behavior. For each assertion, provide the complete JSON object with all fields:
- id, description, category, severity, target (with criteria from the actual JSX), assertionType, precondition (if needed), source: "ai-generated", reviewed: false, enabled: true

Focus on:
1. **Behavioral assertions** grounded in actual code — describe what happens when the user clicks a button, submits a form, or triggers a state change, based on what the code actually does
2. **Semantic assertions** for complex flows — describe multi-step operations (creation flows, data pipelines, library calls) with expected intermediate and final results
3. **State-dependent assertions** with preconditions — things that only appear after specific user actions
4. **Error state assertions** — find error handling in the code and write assertions for what the user should see
5. **Data integrity assertions** — verify that the data displayed matches what the API returns or what the data model defines

Output each assertion as a complete JSON object inside a code block so it can be directly applied.`,
  };

  return `${specContext}\n\n---\n\n${reviewInstructions[reviewType]}`;
}
