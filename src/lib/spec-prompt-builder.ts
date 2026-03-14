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

## CRITICAL: Communicate Progress During Exploration

**Output brief progress notes as you work.** After each major exploration step, write a short summary of what you found. This serves two purposes:
1. The user can see what you're doing (instead of a silent "working..." indicator)
2. If the session is interrupted, your exploration is captured

**Format:** After each file or group of related files you read, write 1-3 sentences summarizing what you learned. For example:
- "📂 **Page component:** StateMachine.tsx renders 4 tabs: States, Transitions, Graph, Settings. Uses useStateMachineConfig() hook for data."
- "📂 **API layer:** Tauri IPC command \`get_state_machine_config\` returns { states, transitions, elements }. Config stored in SQLite."
- "📂 **Domain logic:** State discovery in \`discovery.rs\` groups elements by co-occurrence across screenshots. Uses cosine similarity clustering."

**Do NOT wait until you've read everything to start writing.** Narrate as you go.

## CRITICAL: Generate Spec Incrementally

**Write each spec group as soon as you have enough information**, rather than reading ALL code first and writing ALL groups at the end. This prevents context overflow on complex pages.

**Pattern:**
1. Read the page component → write the "Page Structure" and "Navigation" groups
2. Read each major section/widget → write that section's group immediately
3. Read domain logic → write the domain logic group immediately
4. At the end, review and add any cross-cutting groups you missed

This incremental approach ensures that even if the context window fills up, most of the spec is already written.

## Read the Source Code Thoroughly

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
- If a page uses **shared components** (e.g., from \`@qontinui/workflow-ui\`), read the component implementations when your assertions describe what those components render — prop types and usage patterns alone don't tell you the full rendered output

The goal is to write semantic assertions that describe expected results with the same precision as someone who has read the code. For example, if a "Discover States" button triggers state discovery in the qontinui library, the spec should describe what states are expected to be discovered, what format they take, and how they appear in the UI — not just "states are discovered."

### CRITICAL: Identify domain logic behind the page

Many pages are **windows into library or backend algorithms**. The page displays the output of domain logic that lives elsewhere. For these pages, the spec must go beyond "does the UI render correctly" and specify **what correct output looks like from the underlying algorithm**.

**How to identify domain logic pages:**
- The page triggers an operation (e.g., "Discover States", "Generate Workflow", "Analyze Code") and displays the result
- The result comes from a library function, backend service, or Rust module — not just a database query
- The page is the primary (or only) way users see the output of that algorithm

**When you find a domain logic page, you MUST:**

1. **Read the algorithm's source code** — not just the API endpoint, but the actual implementation. Follow the call chain from the UI action into the library or module that does the computation. Understand the algorithm's inputs, processing steps, and output data structures.

2. **Define correctness criteria as semantic assertions** — write assertions that specify what CORRECT output looks like. These are the specification of the algorithm. They should be precise enough that an AI could:
   - Determine whether the algorithm's output is correct or incorrect by examining the page
   - Fix the algorithm in the library if the output doesn't match the spec
   - Implement the algorithm from scratch using only the spec

3. **Include algorithm invariants** — properties that must always hold regardless of input. Example: "Every state must contain at least one element", "No two states should have identical element sets", "The total element count across states equals the number of unique elements discovered."

4. **Include input-output relationships** — describe how inputs map to outputs. Example: "Given N screenshots with K distinct element configurations, the algorithm should produce exactly K states."

5. **Include edge cases and error modes** — describe what should happen with unusual inputs. Example: "If all screenshots have identical elements, exactly one state should be created with no transitions."

6. **Describe data structures visible in the UI** — what fields, properties, and relationships should the displayed data have.

**The goal: if someone reads ONLY the spec (not the source code), they should understand what the algorithm does, what correct output looks like, and how to verify it.** The spec is the authoritative definition of correct behavior — spec-driven development and debugging workflows depend on this.

### CONSTRAINT: Every assertion must be verifiable from the page

**Every assertion must be something that can be verified by examining the page.** The page is the only thing a verification workflow can observe. The verification loop works like this: the algorithm produces output → the page displays it → a workflow reads the spec and examines the page → the workflow determines if the output matches the spec.

This means:
- **Write assertions in terms of what the page SHOWS, not what the code DOES internally.**
- **Good:** "The state list should display exactly K states, where K is the number of distinct element configurations. Each state card shows its element count and the list of interactive elements that define it."
- **Bad:** "The clustering algorithm uses cosine similarity with a threshold of 0.85."
- If an assertion describes something not visible on the page, reframe it in terms of what IS visible, or drop it — it belongs in unit tests, not page specs.

### What to extract from code for specs:
- **Rendered text** — headings, button labels, tab names, placeholder text, empty state messages, error messages
- **Conditional rendering** — what shows/hides based on state? What are the empty/loading/error states?
- **User interactions** — what can the user click, type, select, drag? What happens when they do?
- **Data flow** — what data is fetched, how is it displayed, what transformations happen?
- **Business logic** — validation rules, required fields, state machine transitions, creation flows
- **Cross-project operations** — what happens in the backend/library when the user triggers an action? What are the expected results?
- **Domain logic outputs** — algorithm results displayed on the page, their expected structure, correctness criteria, and invariants

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

## Example: Domain Logic Group (State Machine Builder)

This example shows how to specify **algorithm correctness** for a page that displays the output of a backend algorithm. The State Machine Builder page triggers state discovery in the qontinui library and displays the resulting states and transitions. The specs define what correct output looks like so a workflow can verify the algorithm by examining the page.

\`\`\`json
{
  "id": "sm-discovery-state-grouping",
  "name": "State Discovery — Grouping Correctness",
  "description": "After running state discovery, the page displays the discovered states in the States tab and Graph tab. Each state represents a distinct UI configuration defined by which interactive elements appear together. The correctness of the grouping algorithm is verifiable by examining the element lists shown on each state card.",
  "category": "state-consistency",
  "assertions": [
    {
      "id": "sm-states-have-elements",
      "description": "Every state card in the state list displays at least one element in its element list. A state with zero elements indicates a grouping error in the discovery algorithm.",
      "category": "state-consistency",
      "severity": "critical",
      "target": { "type": "search", "criteria": { "role": "list" }, "label": "State list" },
      "assertionType": "semantic",
      "precondition": "Discovery has completed and at least one state exists",
      "source": "manual", "reviewed": true, "enabled": true
    },
    {
      "id": "sm-states-distinct-elements",
      "description": "No two states in the state list should display identical element sets. Each state card shows its element IDs — if two states show the exact same elements, the discovery algorithm failed to merge them into a single state. Compare the element lists across all state cards.",
      "category": "state-consistency",
      "severity": "critical",
      "target": { "type": "search", "criteria": { "role": "list" }, "label": "State list" },
      "assertionType": "semantic",
      "precondition": "Discovery has completed and at least two states exist",
      "source": "manual", "reviewed": true, "enabled": true
    },
    {
      "id": "sm-element-coverage",
      "description": "The total number of unique elements across all states should equal the config's element_count shown in the header. If the sum is less, some elements were dropped during discovery. If more, elements were duplicated across states.",
      "category": "state-consistency",
      "severity": "critical",
      "target": { "type": "search", "criteria": { "role": "list" }, "label": "State list" },
      "assertionType": "semantic",
      "precondition": "Discovery has completed and config header is visible",
      "source": "manual", "reviewed": true, "enabled": true
    },
    {
      "id": "sm-transitions-connect-states",
      "description": "Each transition in the Transitions tab shows a source state and target state, both of which must exist in the States list. The triggering action (click, type, navigate) and its target element must be shown. A transition pointing to a nonexistent state indicates a bug in the transition detection algorithm.",
      "category": "state-consistency",
      "severity": "critical",
      "target": { "type": "search", "criteria": { "role": "list" }, "label": "Transition list" },
      "assertionType": "semantic",
      "precondition": "Discovery has completed and at least one transition exists",
      "source": "manual", "reviewed": true, "enabled": true
    },
    {
      "id": "sm-graph-matches-data",
      "description": "The Graph tab renders one node per state and one edge per transition. The node count must match the state list count. Each edge connects two nodes corresponding to the transition's from/to states. Missing nodes or edges indicate a rendering bug; extra nodes indicate a data inconsistency.",
      "category": "state-consistency",
      "severity": "critical",
      "target": { "type": "search", "criteria": { "role": "graphics-document" }, "label": "State machine graph" },
      "assertionType": "semantic",
      "precondition": "Graph tab is selected and discovery has completed",
      "source": "manual", "reviewed": true, "enabled": true
    }
  ],
  "source": "manual"
}
\`\`\`

Key points about domain logic groups:
- Every assertion is **verifiable from the page** — it describes what you can SEE (element lists on cards, confidence badges, node/edge counts in the graph)
- Assertions define **what correct algorithm output looks like** — an AI reading these can determine if the discovery algorithm is broken
- Assertions explain **what a failure means** — "if two states show identical elements, the algorithm failed to merge them" tells the AI exactly what to fix
- The \`state-consistency\` category signals these are data correctness checks, not just UI layout checks

### CSS & Canvas Assertion Guidance

**CSS variable resolution:** Computed CSS styles depend on CSS variable resolution (e.g., \`text-text-primary\` resolves differently in dark/light mode). Exact hex values in \`cssProperty\` assertions are fragile — prefer \`semantic\` assertions describing the visual intent, or note the design token name in the description.

**Canvas and graph-based UIs:** ReactFlow, SVG canvases, and \`<canvas>\` elements render content outside the standard DOM tree. Standard DOM queries cannot reliably count nodes or verify edges. For graph/canvas UIs, use \`semantic\` assertions describing expected visual structure rather than \`count\` or \`exists\` targeting internal SVG/canvas children.

## Output Format

**Output the spec as a single JSON code block** wrapped in \`\`\`json ... \`\`\` fences.

**IMPORTANT: Write spec groups incrementally.** As you explore each area of the codebase, immediately write the corresponding spec group(s). Do NOT accumulate all exploration notes and write the entire spec at the end — this risks context overflow on complex pages.

**Recommended workflow:**
1. Read the page component → immediately output progress notes about page structure
2. For each major section/feature: read the code → output progress notes → mentally compose the group
3. After exploring the full page, output the complete JSON spec with all groups

## Instructions

1. **Read the source code thoroughly** — explore components, hooks, API calls, and data models. Output progress notes as you go.
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

// =============================================================================
// Spec merge (updating existing specs)
// =============================================================================

/**
 * Instructions for AI-driven spec merge — appended when updating existing specs.
 * Teaches the AI to compare old vs new and keep the more detailed version.
 */
export const SPEC_MERGE_INSTRUCTIONS = `
## Merge Mode — Updating an Existing Spec

You are UPDATING an existing spec, not creating from scratch. The current spec is provided below.

### Merge Rules (CRITICAL — follow these exactly)

1. **Read the source code first** (same process as creation — explore components, hooks, API calls)
2. **Compare each existing group against what you find in the code:**
   - If an existing group covers functionality that still exists → KEEP it, enhance if you can add detail
   - If an existing group covers functionality that was removed → REMOVE it
   - If you find functionality not covered by any existing group → ADD a new group
3. **For individual assertions within kept groups:**
   - KEEP assertions that are still accurate — preserve their IDs
   - ENHANCE assertions that are correct but could be more specific or detailed — keep the same ID
   - REMOVE assertions only if the feature they describe no longer exists
   - ADD new assertions for uncovered functionality — give them new unique IDs
4. **NEVER reduce detail** — if an existing assertion has a detailed behavioral description, your replacement must be AT LEAST as detailed. When in doubt, keep the existing version.
5. **Preserve assertion IDs** for assertions that haven't meaningfully changed — this enables tracking changes over time

### Output Format

Output a SINGLE complete JSON spec (the merged result). After the JSON block, add a brief merge summary:

\`\`\`
### Merge Summary
- **Kept**: X groups unchanged, Y assertions unchanged
- **Enhanced**: X groups improved, Y assertions made more detailed
- **Added**: X new groups, Y new assertions
- **Removed**: X groups (list reasons), Y assertions (list reasons)
\`\`\`
`.trim();

/**
 * Build a creation prompt that includes existing spec context for merge.
 * Used when generating/regenerating specs for a page that already has specs.
 */
export function buildSpecCreationWithMergeContext(
  existingSpec: { specId: string; config: SpecConfig; appName?: string },
  pageDescription: string,
): string {
  const existingContext = buildDetailedSpecContext(existingSpec);
  const existingJson = JSON.stringify(existingSpec.config, null, 2);

  return [
    SPEC_CREATION_INSTRUCTIONS,
    "",
    "---",
    "",
    SPEC_MERGE_INSTRUCTIONS,
    "",
    "---",
    "",
    "## Current Spec to Merge With",
    "",
    existingContext,
    "",
    "### Raw JSON (preserve IDs from this when keeping assertions)",
    "",
    "```json",
    existingJson,
    "```",
    "",
    "---",
    "",
    `Create an updated, comprehensive page spec by merging improvements with the existing spec above.`,
    "",
    pageDescription,
  ].join("\n");
}
