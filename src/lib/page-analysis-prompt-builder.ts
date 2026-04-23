/**
 * page-analysis-prompt-builder.ts
 *
 * Builds AI prompts for the unified repository preparation tool.
 * Generates three types of output per page:
 * 1. useUIElement/useUIComponent registrations (semantic element registration)
 * 2. .spec.uibridge.json page specs (assertions for all categories)
 * 3. Tutorial data files (informational tutorials)
 *
 * Reuses SPEC_CREATION_INSTRUCTIONS and SPEC_MERGE_INSTRUCTIONS from
 * spec-prompt-builder.ts for spec generation quality.
 */

import { SPEC_CREATION_INSTRUCTIONS, SPEC_MERGE_INSTRUCTIONS } from "./spec-prompt-builder";

// =============================================================================
// Registration Prompt
// =============================================================================

const REGISTRATION_INSTRUCTIONS = `You are generating UI Bridge element registrations for a React page component.

## Goal

Register every significant interactive and data-display element with the UI Bridge SDK so that AI agents can discover, inspect, and interact with them programmatically.

## Output Format

Generate a single TypeScript file that:
1. Imports \`useUIComponent\` and \`useUIElement\` from "@qontinui/ui-bridge"
2. Exports a custom hook \`use<PageName>Registrations()\` that:
   - Calls \`useUIComponent\` once for the page-level component with:
     - \`id\`: kebab-case page identifier (e.g., "dashboard-page")
     - \`name\`: Human-readable page name
     - \`description\`: What the page does
     - \`actions\`: Array of page-level actions the user can perform (with handler stubs)
   - Calls \`useUIElement\` for each significant element with:
     - \`id\`: kebab-case unique identifier (e.g., "search-input", "submit-button")
     - \`type\`: "button" | "input" | "select" | "container" | "link" | "heading" | "table"
     - \`label\`: Human-readable description of what this element is/does
     - \`actions\`: Array of standard actions (["click"] for buttons, etc.)
   - Returns an object of all refs: \`{ searchInputRef, submitButtonRef, ... }\`
3. The calling page component destructures these refs and attaches them to DOM elements

## What to Register

### MUST register (interactive):
- All buttons (with action descriptions)
- All text inputs and textareas (with purpose labels)
- All select/dropdown elements
- All toggles, checkboxes, radio buttons
- Navigation links and tabs
- Form submit/reset actions

### MUST register (data display):
- Data tables and lists (as containers with row count in label)
- Key metric displays
- Status indicators
- Error/empty state containers

### SHOULD register:
- Section headings (for navigation context)
- Modal/dialog containers (when open)
- Tab panels

### DO NOT register:
- Individual list items in a dynamic list (register the container instead)
- Decorative icons
- Layout wrappers with no semantic meaning
- Elements inside third-party component libraries that you can't attach refs to

## Naming Conventions

- IDs: kebab-case, prefixed with page context (e.g., "dashboard-search-input")
- Labels: Human-readable, describe purpose not implementation
  - Good: "Search workflows by name"
  - Bad: "text input element"
- Actions: Use verb phrases
  - Good: "Search", "Create New Workflow", "Toggle Filters"
  - Bad: "click", "action1"

## File Header

Start the file with:
\`\`\`
// FILE: src/lib/ui-bridge/pages/<page-name>-registrations.tsx
\`\`\`

This marker is used to extract the file from your response.`;

/**
 * A `useUIElement` / `useUIComponent` call found inline in page source.
 * Captured so the registration prompt can tell the LLM "these elements are
 * already tagged; don't re-emit them in the generated side-file."
 */
export interface InlineRegistration {
  /** Which hook was called: `useUIElement` or `useUIComponent`. */
  hook: "useUIElement" | "useUIComponent";
  /** The `id:` string literal. */
  id: string;
  /** The `label:` or `name:` string literal, if present. */
  label?: string;
  /** The `type:` string literal, if present. */
  type?: string;
}

/**
 * Regex-scan a page source for inline `useUIElement(...)` / `useUIComponent(...)`
 * calls and extract the `id` (always) plus `label`/`name` and `type` (when
 * present). Robust enough for common call shapes; doesn't attempt to handle
 * computed keys or template literals.
 *
 * The output is a summary the AI prompt uses to know which elements are
 * already covered inline — so the side-file it generates doesn't duplicate.
 */
export function extractInlineRegistrations(pageSource: string): InlineRegistration[] {
  const results: InlineRegistration[] = [];
  // Match either hook followed by (  { ... } up to the next `})` that isn't
  // inside a nested object. Simple non-greedy scan works for the shapes the
  // runner's existing registrations use. We walk with an index + brace
  // counter rather than a single regex to handle nested braces correctly.
  const hookPattern = /\b(useUIElement|useUIComponent)\s*\(\s*\{/g;
  let match: RegExpExecArray | null;
  while ((match = hookPattern.exec(pageSource)) !== null) {
    const hook = match[1] as InlineRegistration["hook"];
    // Position right after the opening brace.
    let i = match.index + match[0].length;
    let depth = 1;
    const start = i;
    while (i < pageSource.length && depth > 0) {
      const ch = pageSource[i];
      if (ch === "{") depth++;
      else if (ch === "}") depth--;
      i++;
    }
    if (depth !== 0) continue; // Unbalanced — skip silently.
    const body = pageSource.slice(start, i - 1);

    const idMatch = /\bid\s*:\s*["'`]([^"'`]+)["'`]/.exec(body);
    if (!idMatch) continue; // No string-literal id — skip (can't summarize a computed id).
    const labelMatch = /\b(?:label|name)\s*:\s*["'`]([^"'`]+)["'`]/.exec(body);
    const typeMatch = /\btype\s*:\s*["'`]([^"'`]+)["'`]/.exec(body);

    results.push({
      hook,
      id: idMatch[1],
      label: labelMatch ? labelMatch[1] : undefined,
      type: typeMatch ? typeMatch[1] : undefined,
    });
  }
  return results;
}

/**
 * Build a prompt for generating useUIElement/useUIComponent registrations.
 *
 * `inlineRegistrations` — if provided, these are `useUIElement`/`useUIComponent`
 * calls already present inline in the page source. The prompt tells the LLM
 * to treat those elements as already covered and NOT to duplicate them in
 * the generated side-file (which would cause runtime double-registration).
 */
export function buildRegistrationPrompt(
  pageSource: string,
  importedSources: Array<{ path: string; content: string }>,
  pageName: string,
  route: string,
  framework: string,
  existingRegistrations?: string,
  inlineRegistrations?: InlineRegistration[],
): string {
  const parts: string[] = [];

  parts.push(REGISTRATION_INSTRUCTIONS);

  parts.push(`\n## Page Information\n`);
  parts.push(`- **Page name:** ${pageName}`);
  parts.push(`- **Route:** ${route}`);
  parts.push(`- **Framework:** ${framework}`);

  parts.push(`\n## Page Component Source\n`);
  parts.push("```tsx");
  parts.push(pageSource);
  parts.push("```");

  if (importedSources.length > 0) {
    parts.push(`\n## Imported Child Components\n`);
    for (const imp of importedSources.slice(0, 5)) {
      parts.push(`### ${imp.path}\n`);
      parts.push("```tsx");
      parts.push(imp.content.slice(0, 8000)); // Cap each import
      parts.push("```\n");
    }
  }

  if (inlineRegistrations && inlineRegistrations.length > 0) {
    parts.push(`\n## Elements Already Tagged Inline (DO NOT RE-EMIT)\n`);
    parts.push(
      "The page component above already contains the following `useUIElement` / `useUIComponent` calls inline. These elements are ALREADY registered by the page itself — if you include them in the generated side-file, runtime will register them twice and the second call will collide on `id`. Emit registrations ONLY for elements that are NOT in this list:\n",
    );
    for (const reg of inlineRegistrations) {
      const label = reg.label ? ` — ${JSON.stringify(reg.label)}` : "";
      const type = reg.type ? ` (type: ${reg.type})` : "";
      parts.push(`- \`${reg.hook}\` id=\`${reg.id}\`${type}${label}`);
    }
    parts.push(
      "\nSpecifically: **skip** any element whose id appears above. If you believe an inline-tagged element needs different metadata, note it in a comment in your output — do NOT re-emit a `useUIElement` / `useUIComponent` call for it.",
    );
  }

  if (existingRegistrations) {
    parts.push(`\n## Existing Registrations (UPDATE MODE)\n`);
    parts.push("The page already has registrations. Compare against the current component source:");
    parts.push("- **Keep** registrations for elements that still exist");
    parts.push("- **Update** labels/descriptions for elements that changed");
    parts.push("- **Add** registrations for new elements");
    parts.push("- **Remove** registrations for deleted elements");
    parts.push("- **Preserve** existing IDs where possible\n");
    parts.push("```tsx");
    parts.push(existingRegistrations);
    parts.push("```");
  }

  parts.push(
    `\n## Output\n\nGenerate the complete registration file. Start with \`\`\`tsx\\n// FILE: src/lib/ui-bridge/pages/${pageName.toLowerCase().replace(/\s+/g, "-")}-registrations.tsx\``,
  );

  return parts.join("\n");
}

// =============================================================================
// Page Spec Prompt
// =============================================================================

/**
 * Build a prompt for generating a .spec.uibridge.json page spec.
 * Reuses SPEC_CREATION_INSTRUCTIONS from spec-prompt-builder.ts.
 */
export function buildPageSpecPrompt(
  pageSource: string,
  importedSources: Array<{ path: string; content: string }>,
  pageName: string,
  route: string,
  registrations: string,
  existingSpec?: string,
): string {
  const parts: string[] = [];

  if (existingSpec) {
    parts.push("You are UPDATING an existing page spec. Follow these merge rules:\n");
    parts.push(SPEC_MERGE_INSTRUCTIONS);
    parts.push(`\n## Existing Spec\n\n\`\`\`json\n${existingSpec}\n\`\`\`\n`);
  }

  parts.push(SPEC_CREATION_INSTRUCTIONS);

  parts.push(`\n## Page Information\n`);
  parts.push(`- **Page name:** ${pageName}`);
  parts.push(`- **Route:** ${route}`);

  parts.push(`\n## Page Component Source\n\n\`\`\`tsx\n${pageSource}\n\`\`\`\n`);

  if (importedSources.length > 0) {
    parts.push(`## Imported Components (first 3)\n`);
    for (const imp of importedSources.slice(0, 3)) {
      parts.push(`### ${imp.path}\n\`\`\`tsx\n${imp.content.slice(0, 5000)}\n\`\`\`\n`);
    }
  }

  parts.push(`## UI Bridge Registrations\n`);
  parts.push(
    "These elements are registered with the UI Bridge. Use their IDs as assertion targets where applicable:\n",
  );
  parts.push(`\`\`\`tsx\n${registrations}\n\`\`\`\n`);

  parts.push(`## Output\n`);
  parts.push("Generate a complete .spec.uibridge.json file. Output it as a JSON code block:\n");
  parts.push("```json\n{ ... }\n```");
  parts.push(`\nSet metadata.pageUrl to "${route}" and metadata.component to "${pageName}".`);
  parts.push(
    "\nInclude groups for ALL applicable categories: element-presence, interaction, data-display, behavior, state-consistency, navigation, semantic, accessibility, layout, design.",
  );
  parts.push(
    "\nIf the page has tabs, modals, panels, or other distinct UI configurations, include a stateMachine section with states and transitions. See the State Machine Section in the instructions above.",
  );

  return parts.join("\n");
}

// =============================================================================
// Tutorial Prompt
// =============================================================================

const TUTORIAL_INSTRUCTIONS = `You are generating an informational tutorial for a Qontinui Runner page.

## Output Format

Generate a TypeScript file that exports a Tutorial object. The file must:
1. Import \`Tutorial\` type from "@/types/tutorial"
2. Export a named constant following the pattern: \`export const <name>Tutorial: Tutorial = { ... }\`
3. Set \`mode: "contextual"\` (runner only supports contextual mode)
4. Use \`data-tutorial-id\` selectors or registered element IDs for targetElement selectors

## Tutorial Structure

Structure the tutorial by the **groups** defined in the page spec. Each group
represents a distinct feature area (e.g. "Workflow Sidebar Library", "Multi-
Phase Editor"). This gives the tutorial a natural rhythm and lets users skim
by section.

For each non-trivial group in the spec, emit:
1. **One section-intro step** with no \`targetElement\` (a centered modal).
   - \`id\`: \`section-<group-id>\`
   - \`title\`: the group's \`name\`
   - \`content\`: start with a 1-sentence summary of the group's \`description\`,
     then a short list of what this section will cover.
2. **1-3 element steps** that spotlight the most important assertions in that
   group. Use the assertion's \`target.label\` or the registered element id for
   \`targetElement.selector\`. Skip trivial assertions (pure layout, aria-only).

Bookends:
- **Start** with ONE centered overview step (no targetElement) explaining what
  the page is and listing the sections the tutorial will visit (echoes the
  group names). Keep to ~4 lines.
- **End** with a "what's next" step pointing to related features elsewhere in
  the runner.

Total target: **8-14 steps**. Skip sections that would add no value (e.g. pure
"state consistency" groups with no user-visible elements).

## Step Content Guidelines

- Use markdown: **bold**, lists, \`code\` — they render in the tooltip
- Keep content to 3-6 lines per step
- Use \`tips\` for non-essential but helpful info
- Use \`details\` for expandable deep-dive content (architecture, algorithms)
- For informational tutorials, focus on WHY things exist, not HOW to click them
- Section-intro steps are purely informational — no action field, no validation

## File Header

Start the file with:
\`\`\`
// FILE: src/components/tutorial/data/<page-name>.ts
\`\`\``;

/**
 * Build a prompt for generating a tutorial data file.
 */
export function buildTutorialPrompt(
  pageSource: string,
  pageName: string,
  route: string,
  registrations: string,
  specSummary: string,
): string {
  const parts: string[] = [];

  parts.push(TUTORIAL_INSTRUCTIONS);

  parts.push(`\n## Page Information\n`);
  parts.push(`- **Page name:** ${pageName}`);
  parts.push(`- **Route:** ${route}`);
  parts.push(`- **Focus page:** "${route.replace(/^\//, "").replace(/\//g, "-") || "help"}"`);

  parts.push(`\n## Page Component Source\n\n\`\`\`tsx\n${pageSource.slice(0, 10000)}\n\`\`\`\n`);

  parts.push(`## Registered UI Bridge Elements\n`);
  parts.push("These elements can be targeted with spotlight/border/pulse highlighting:\n");
  parts.push(`\`\`\`tsx\n${registrations}\n\`\`\`\n`);

  parts.push(`## Page Spec Summary\n`);
  parts.push("The spec describes what this page does (use this to understand the feature):\n");
  parts.push(specSummary.slice(0, 5000));

  parts.push(`\n## Output\n`);
  parts.push(
    `Generate the tutorial file. Start with \`\`\`ts\\n// FILE: src/components/tutorial/data/${pageName.toLowerCase().replace(/\s+/g, "-")}.ts\``,
  );

  return parts.join("\n");
}

// =============================================================================
// Architecture Diagram
// =============================================================================

const ARCHITECTURE_DIAGRAM_INSTRUCTIONS = `
# Page Architecture Diagram

Generate a **Mermaid flowchart** documenting the structural architecture of a
single page component. The diagram should answer: "what is this page made of,
where does its data come from, and what does it call out to?"

## What to include

Group nodes into these labelled subgraphs (omit a subgraph when empty):

1. **Page** — the top-level component itself (one node).
2. **Components** — imported React components the page renders (child panels,
   modals, list items). Only include meaningful custom components; skip tiny
   primitives (Button, Icon, Loader, etc.).
3. **State** — React context providers it reads from (via useX hooks),
   custom hooks that hold state (useReducer, useQuery), and local store
   references (Zustand, Jotai, etc.).
4. **Services / API** — backend HTTP endpoints it fetches (list the route,
   not the payload), GraphQL queries/mutations by name, Tauri invoke commands
   by command name, and WebSocket / SSE streams.
5. **External** — links to other pages the user can navigate to, and anything
   that leaves the app (postMessage, window.open, external URLs).

## Edge conventions

- Page → Component: solid arrow \`-->\` labelled "renders" (label optional).
- Component → State: dotted arrow \`-.->\` labelled "reads" or "writes".
- Component / Page → API: solid arrow with the HTTP verb in the label,
  e.g. \`-->|POST|\`.
- Any node → External: thick arrow \`==>\` labelled "navigates" or "opens".

## Output format

Emit exactly one fenced \`\`\`mermaid block, preceded by a \`// FILE:\` comment
line so the writer knows where to save it. No prose outside the block. Example
shape:

\`\`\`mermaid
%% FILE: src/specs/architecture/<page-slug>.arch.mmd
flowchart TD
  subgraph Page
    P["<PageName>"]
  end
  subgraph Components
    C1["ChildPanel"]
    C2["DetailView"]
  end
  subgraph State
    S1[("useTaskRuns")]
    S2[("AuthContext")]
  end
  subgraph Services
    A1{{"GET /task-runs"}}
    A2{{"invoke: start_workflow"}}
  end
  subgraph External
    X1(("/results"))
  end

  P --> C1
  P --> C2
  C1 -.->|reads| S1
  C1 -->|GET| A1
  C2 -.->|reads| S2
  C2 -->|POST| A2
  P ==>|navigates| X1
\`\`\`

Keep the diagram readable: aim for 5-20 nodes total. Collapse noise into
grouped subgraph labels if needed. Do not hallucinate endpoints or state —
only include things you can see in the source.
`;

export function buildArchitectureDiagramPrompt(
  pageSource: string,
  importedSources: Array<{ path: string; content: string }>,
  pageName: string,
  route: string,
  registrations: string,
): string {
  const parts: string[] = [];
  parts.push(ARCHITECTURE_DIAGRAM_INSTRUCTIONS);

  parts.push(`\n## Page Information\n`);
  parts.push(`- **Page name:** ${pageName}`);
  parts.push(`- **Route:** ${route}`);
  const slug = route.replace(/^\//, "").replace(/\//g, "-") || "root";
  parts.push(`- **Output filename:** src/specs/architecture/${slug}.arch.mmd`);

  parts.push(`\n## Page Component Source\n\n\`\`\`tsx\n${pageSource.slice(0, 10000)}\n\`\`\`\n`);

  if (importedSources.length > 0) {
    parts.push(`\n## Imported Component Sources (trimmed)\n`);
    for (const imp of importedSources.slice(0, 8)) {
      parts.push(`\n### ${imp.path}\n\n\`\`\`tsx\n${imp.content.slice(0, 2500)}\n\`\`\``);
    }
  }

  if (registrations) {
    parts.push(
      `\n## UI Bridge Registrations\n\n\`\`\`tsx\n${registrations.slice(0, 3000)}\n\`\`\``,
    );
  }

  parts.push(`\n## Output\n`);
  parts.push(
    `Emit exactly one \`\`\`mermaid block that begins with the \`%% FILE:\` comment. ` +
      `No text before or after the block.`,
  );

  return parts.join("\n");
}

// =============================================================================
// Project Explainer (hierarchical, multi-file)
// =============================================================================
//
// Three-pass generation to keep each AI turn bounded and easy to review:
//   1. Index       — one prompt, one file: overview + cluster links
//   2. Cluster     — one prompt per cluster: narrative + links to its pages
//   3. Page        — one prompt per page: deep dive + embedded arch diagram
//
// All outputs are standalone .md files under src/specs/explainer/. The viewer
// navigates hierarchically (index → cluster → page) using the markdown links
// the AI writes; no extra metadata file is needed.

export interface ExplainerSpecSummary {
  specId: string;
  description: string;
  /** Group names + one-line descriptions, so clustering can be inferred. */
  groups: Array<{ id: string; name: string; description: string }>;
}

export interface ExplainerCluster {
  /** Short slug used for filenames — e.g. "orchestration". */
  id: string;
  /** Human-readable title. */
  name: string;
  /** One-paragraph description. */
  description: string;
  /** Spec IDs belonging to this cluster. */
  specIds: string[];
}

const EXPLAINER_INDEX_INSTRUCTIONS = `You are writing the **index** of a project's hierarchical explainer — a
navigable documentation surface generated from per-page specs and architecture
diagrams. Readers land here first, so the index must orient them before they
drill down.

## Output structure (single markdown file)

1. \`# <Project name>\` — the project's human-readable title.
2. **Overview** (2-3 paragraphs, H2): what the project does, who it's for,
   the core concepts (proper nouns) a reader should hold in their head before
   reading anything else.
3. **How this explainer is organized** (short paragraph): tell the reader they
   will find high-level clusters here, then per-page deep-dives inside each
   cluster.
4. **Clusters** (H2), then one H3 per cluster:
   - Cluster title
   - 2-4 sentence description (what the pages in this cluster do *together*).
   - A "Pages in this cluster" bulleted list linking to each page's file:
     \`- [<page title>](./<cluster-id>/<page-slug>.md) — <one-line tagline>\`.
   - A link to the cluster's own overview: \`[Read more →](./<cluster-id>.md)\`.
5. **Glossary** (H2, optional): recurring proper nouns from the specs,
   each with a 1-sentence definition. Skip if nothing recurs.

## Tone

- Write for a new engineer joining the project. Assume intelligence, not
  context. Explain *why* each part exists before *how* it works.
- Avoid marketing copy. Prefer concrete verbs ("executes workflows",
  "records interactions") over vague ones ("enables", "empowers").

## File header

Begin the output with a single HTML comment on its own line:
\`<!-- FILE: src/specs/explainer/index.md -->\`
Then the markdown content.`;

export function buildExplainerIndexPrompt(
  projectName: string,
  specs: ExplainerSpecSummary[],
  clusters: ExplainerCluster[],
): string {
  const parts: string[] = [];
  parts.push(EXPLAINER_INDEX_INSTRUCTIONS);

  parts.push(`\n## Project\n`);
  parts.push(`- **Name:** ${projectName}`);

  parts.push(`\n## Proposed clusters\n`);
  parts.push(
    "Use these cluster groupings. If one is clearly wrong, merge or rename it, but keep the total between 3 and 8 clusters.\n",
  );
  for (const c of clusters) {
    parts.push(`\n### ${c.name} (id: \`${c.id}\`)`);
    parts.push(c.description);
    parts.push(`Spec IDs: ${c.specIds.map((s) => `\`${s}\``).join(", ")}`);
  }

  parts.push(`\n## All spec descriptions\n`);
  for (const s of specs) {
    parts.push(`- **${s.specId}** — ${s.description.replace(/\s+/g, " ").slice(0, 220)}`);
  }

  parts.push(`\n## Output\n`);
  parts.push(
    "Emit exactly one markdown file. Start with the `<!-- FILE: src/specs/explainer/index.md -->` comment.",
  );
  return parts.join("\n");
}

const EXPLAINER_CLUSTER_INSTRUCTIONS = `You are writing the cluster-level page of a hierarchical project explainer.
The reader came here from the index and wants a narrative explanation of a
group of related pages *before* drilling into any one page.

## Output structure

1. \`# <Cluster name>\` at the top.
2. **What this cluster does** (H2, 2-4 paragraphs): the *shared purpose* of
   these pages. Explain the user journey across them, not page-by-page.
3. **How the pieces fit together** (H2): walk the reader through the
   interactions between pages in this cluster. Cite page names (with links)
   but keep the narrative shape.
4. **Pages in this cluster** (H2): one H3 per page, each with:
   - A one-sentence tagline of that page's role *within this cluster*.
   - A link: \`[Read full page explainer →](./<cluster-id>/<page-slug>.md)\`.
5. **Connects to** (H2, bulleted): other clusters this one reads from or
   hands off to, with links: \`- [<other cluster>](./<other-id>.md) — why\`.

## Tone

Same as the index: concrete, jargon only where it refers to a real concept,
assume an attentive new-joiner reader.

## File header

Begin with \`<!-- FILE: src/specs/explainer/<cluster-id>.md -->\`.`;

export function buildExplainerClusterPrompt(
  projectName: string,
  cluster: ExplainerCluster,
  specsInCluster: ExplainerSpecSummary[],
  otherClusters: Array<{ id: string; name: string; description: string }>,
): string {
  const parts: string[] = [];
  parts.push(EXPLAINER_CLUSTER_INSTRUCTIONS);

  parts.push(`\n## Project\n`);
  parts.push(`- **Name:** ${projectName}`);

  parts.push(`\n## Cluster\n`);
  parts.push(`- **Name:** ${cluster.name}`);
  parts.push(`- **Id (for filenames):** \`${cluster.id}\``);
  parts.push(`- **Description:** ${cluster.description}`);

  parts.push(`\n## Specs in this cluster\n`);
  for (const s of specsInCluster) {
    parts.push(`\n### ${s.specId}\n`);
    parts.push(s.description.replace(/\s+/g, " ").slice(0, 600));
    if (s.groups.length > 0) {
      parts.push(`\nGroups:`);
      for (const g of s.groups.slice(0, 8)) {
        parts.push(`- **${g.name}** — ${g.description.replace(/\s+/g, " ").slice(0, 160)}`);
      }
    }
  }

  parts.push(`\n## Sibling clusters (for "Connects to" links)\n`);
  for (const c of otherClusters) {
    parts.push(`- \`${c.id}\` — ${c.name}: ${c.description.slice(0, 140)}`);
  }

  parts.push(`\n## Output\n`);
  parts.push(
    `Emit exactly one markdown file. Start with the \`<!-- FILE: src/specs/explainer/${cluster.id}.md -->\` comment.`,
  );
  return parts.join("\n");
}

const EXPLAINER_PAGE_INSTRUCTIONS = `You are writing the deep-dive explainer for a single page. The reader
landed here from a cluster overview and wants to understand this specific
page in depth.

## Output structure

1. \`# <Page title>\` at the top.
2. **Purpose** (H2, 1-2 paragraphs): what this page does and who uses it.
3. **Key interactions** (H2): for each meaningful spec *group*, an H3 with:
   - 1-2 sentences of narrative
   - A short bulleted list of the concrete controls / outcomes
   (Skip trivial groups like pure layout or accessibility assertions.)
4. **Architecture** (H2): render the page's architecture diagram as an
   inline Mermaid codeblock if one was provided in the context. Follow the
   diagram with 2-4 sentences describing the data flows it represents.
5. **Connects to** (H2): other pages in the same cluster (or nearby
   clusters) that this page reads from / hands off to, with relative links.

## Tone

Concrete, source-grounded. If the spec doesn't say something, don't
invent it. If an assertion's purpose is unclear from its description, omit
it rather than speculating.

## File header

Begin with \`<!-- FILE: src/specs/explainer/<cluster-id>/<page-slug>.md -->\`.`;

export function buildExplainerPagePrompt(
  projectName: string,
  clusterId: string,
  pageSlug: string,
  spec: ExplainerSpecSummary,
  archDiagram: string | null,
  siblingPages: Array<{ slug: string; title: string; tagline: string }>,
): string {
  const parts: string[] = [];
  parts.push(EXPLAINER_PAGE_INSTRUCTIONS);

  parts.push(`\n## Project\n`);
  parts.push(`- **Name:** ${projectName}`);

  parts.push(`\n## Page\n`);
  parts.push(`- **Spec id:** ${spec.specId}`);
  parts.push(`- **Cluster:** ${clusterId}`);
  parts.push(`- **Output filename:** src/specs/explainer/${clusterId}/${pageSlug}.md`);
  parts.push(`- **Description:** ${spec.description}`);

  parts.push(`\n## Groups in this page's spec\n`);
  for (const g of spec.groups) {
    parts.push(`\n### ${g.name}`);
    parts.push(g.description.replace(/\s+/g, " ").slice(0, 500));
  }

  if (archDiagram) {
    parts.push(`\n## Architecture diagram (embed this verbatim in the Architecture section)\n`);
    parts.push("```mermaid");
    parts.push(archDiagram.trim());
    parts.push("```");
  } else {
    parts.push(
      `\n## Architecture diagram\n\n_No diagram available; omit the Architecture section._`,
    );
  }

  if (siblingPages.length > 0) {
    parts.push(`\n## Sibling pages in this cluster (for "Connects to" links)\n`);
    for (const p of siblingPages) {
      parts.push(`- \`${p.slug}.md\` — **${p.title}**: ${p.tagline}`);
    }
  }

  parts.push(`\n## Output\n`);
  parts.push(
    `Emit exactly one markdown file. Start with ` +
      `\`<!-- FILE: src/specs/explainer/${clusterId}/${pageSlug}.md -->\`.`,
  );
  return parts.join("\n");
}

// Re-exported for convenience — also available from spec-prompt-builder.ts directly
export { SPEC_CREATION_INSTRUCTIONS, SPEC_MERGE_INSTRUCTIONS };
