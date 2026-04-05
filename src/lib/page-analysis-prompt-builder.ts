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
1. Imports \`useUIComponent\` and \`useUIElement\` from "ui-bridge"
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
 * Build a prompt for generating useUIElement/useUIComponent registrations.
 */
export function buildRegistrationPrompt(
  pageSource: string,
  importedSources: Array<{ path: string; content: string }>,
  pageName: string,
  route: string,
  framework: string,
  existingRegistrations?: string,
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

- **8-12 steps** — enough to cover the feature without being overwhelming
- **Start** with a centered modal (no targetElement) explaining what the page is
- **Middle steps** spotlight specific UI elements to explain their purpose
- **End** with a "what's next" step pointing to related features
- Each step should take 30-90 seconds to read

## Step Content Guidelines

- Use markdown: **bold**, lists, \`code\` — they render in the tooltip
- Keep content to 3-6 lines per step
- Use \`tips\` for non-essential but helpful info
- Use \`details\` for expandable deep-dive content (architecture, algorithms)
- For informational tutorials, focus on WHY things exist, not HOW to click them

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

// Re-exported for convenience — also available from spec-prompt-builder.ts directly
export { SPEC_CREATION_INSTRUCTIONS, SPEC_MERGE_INSTRUCTIONS };
