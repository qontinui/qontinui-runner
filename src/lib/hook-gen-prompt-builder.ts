/**
 * hook-gen-prompt-builder.ts
 *
 * Builds AI prompts for generating UI Bridge hook integration code.
 * The AI reads the project's source code and generates app-specific
 * hook files for route awareness, page context, state machine, etc.
 */

// =============================================================================
// Types
// =============================================================================

export type HookCategory =
  | "route-awareness"
  | "page-context"
  | "state-machine"
  | "components"
  | "keyboard-shortcuts"
  | "undo-redo"
  | "annotations"
  | "intents";

export const ALL_HOOK_CATEGORIES: HookCategory[] = [
  "route-awareness",
  "page-context",
  "state-machine",
  "components",
  "keyboard-shortcuts",
  "undo-redo",
  "annotations",
  "intents",
];

export const HOOK_CATEGORY_LABELS: Record<HookCategory, string> = {
  "route-awareness": "Route Awareness",
  "page-context": "Page Context",
  "state-machine": "State Machine (UI States & Transitions)",
  components: "Component Actions & State",
  "keyboard-shortcuts": "Keyboard Shortcuts",
  "undo-redo": "Undo/Redo",
  annotations: "Element Annotations",
  intents: "Intents",
};

export const HOOK_CATEGORY_DESCRIPTIONS: Record<HookCategory, string> = {
  "route-awareness":
    "Router integration for navigation tracking — route patterns, params, query strings",
  "page-context": "Semantic page names, sections, breadcrumbs derived from routes",
  "state-machine": "Modals, panels, tabs, auth states, form steps — with transitions between them",
  components: "Component-level actions, state getters, computed properties",
  "keyboard-shortcuts": "Non-ARIA keyboard shortcuts from event handlers or hotkey libraries",
  "undo-redo": "Wiring to the app's state management undo/redo system",
  annotations: "Semantic metadata for specific UI elements",
  intents: "Named composite user actions (e.g., 'create-new-project', 'submit-form')",
};

// =============================================================================
// Hook API Signatures (embedded from SDK source)
// =============================================================================

const HOOK_API_REFERENCE = `
## UI Bridge Hook API Reference

### useRouteAwareness(info: RouteInfo): void
Provides framework-router integration for the navigation tracker.

\`\`\`typescript
interface RouteInfo {
  pattern?: string;              // Route pattern (e.g., "/tasks/:id")
  params?: Record<string, string>;  // Extracted route parameters
  queryParams?: Record<string, string>;  // Query parameters
  routeStack?: string[];         // Matched route stack / breadcrumb
}
\`\`\`

### usePageContext(context: DeveloperPageContext): void
Annotates the current page with semantic context for AI automation.

\`\`\`typescript
interface DeveloperPageContext {
  name: string;           // Semantic page name (e.g., "Task Detail", "Dashboard")
  section?: string;       // Application section/area (e.g., "tasks", "settings")
  breadcrumb?: string[];  // Breadcrumb trail (e.g., ["Tasks", "Task #123"])
  meta?: Record<string, unknown>;  // Arbitrary metadata
}
\`\`\`

### useUIState(options: UseUIStateOptions): UseUIStateReturn
Registers a UI state with UI Bridge for state management.

\`\`\`typescript
interface UseUIStateOptions {
  id: string;                    // Unique identifier for the state
  name: string;                  // Human-readable name
  elements?: string[];           // Element IDs belonging to this state
  activeWhen?: () => boolean;    // Function to detect if state is active
  blocking?: boolean;            // If true, blocks other state activations (modal)
  blocks?: string[];             // Specific state IDs this state blocks
  group?: string;                // State group membership
  metadata?: Record<string, unknown>;
}
\`\`\`

### useUITransition(options: UseUITransitionOptions): UseUITransitionReturn
Registers a state transition with UI Bridge.

\`\`\`typescript
interface UseUITransitionOptions {
  id: string;                    // Unique identifier
  name: string;                  // Human-readable name
  fromStates: string[];          // Precondition: at least one must be active
  activateStates: string[];      // States to activate
  exitStates: string[];          // States to deactivate
  actions?: WorkflowStep[];      // Actions to execute during transition
}
\`\`\`

### useUIStateGroup(options: UseUIStateGroupOptions): UseUIStateGroupReturn
Groups related states together (e.g., tabs, wizard steps).

\`\`\`typescript
interface UseUIStateGroupOptions {
  id: string;                    // Unique group identifier
  name: string;                  // Human-readable name
  stateIds: string[];            // IDs of states in this group
  exclusive?: boolean;           // If true, only one state can be active at a time
}
\`\`\`

### useUIComponent(options: UseUIComponentOptions): UseUIComponentReturn
Registers a component with UI Bridge for component-level actions.

\`\`\`typescript
interface UseUIComponentOptions {
  id: string;                    // Unique component identifier
  name: string;                  // Human-readable name
  description?: string;
  actions?: ComponentActionDef[];  // Actions available on this component
  elementIds?: string[];         // Child element IDs owned by this component
  state?: () => Record<string, unknown>;  // Function to get current state
  computed?: Record<string, ComputedPropertyDef | (() => unknown)>;
}

interface ComponentActionDef<TParams = unknown, TResult = unknown> {
  id: string;
  label?: string;
  description?: string;
  handler: (params?: TParams) => TResult | Promise<TResult>;
}
\`\`\`

### useKeyboardShortcuts(shortcuts: ShortcutDef[]): void
Registers keyboard shortcuts with UI Bridge so AI agents can discover them.

\`\`\`typescript
type ShortcutDef = {
  combo: string;               // e.g., "Ctrl+S", "Ctrl+Shift+N"
  description: string;         // Human-readable description
  scope?: string;              // Scope (e.g., "editor", "global")
  category?: string;           // Category for grouping
};
\`\`\`

### useUndoRedo(options: DeclaredUndoState): void
Declares undo/redo state to the UI Bridge (overrides heuristic detection).

\`\`\`typescript
interface DeclaredUndoState {
  canUndo: boolean;
  canRedo: boolean;
  undoDescription?: string;
  redoDescription?: string;
  undoStack?: string[];        // Full undo stack descriptions (most recent first)
  redoStack?: string[];
  onUndo?: () => void;         // Execute undo programmatically
  onRedo?: () => void;
}
\`\`\`

### useUIAnnotation(elementId: string, annotation: ElementAnnotation): void
Registers a semantic annotation for a UI element.

\`\`\`typescript
interface ElementAnnotation {
  description?: string;        // What this element is
  purpose?: string;            // Why it exists
  notes?: string;              // Behavioral notes, edge cases
  tags?: string[];             // Searchable tags
  relatedElements?: string[];  // IDs of related elements
  metadata?: Record<string, unknown>;
}
\`\`\`

### registerIntent (via POST /ai/intents/register)
Registers a named composite user action.

\`\`\`typescript
interface Intent {
  id: string;                  // Unique intent identifier
  name: string;                // Human-readable name
  description: string;         // What the intent does
  tags?: string[];             // Tags for categorization
  params?: Record<string, IntentParam>;
  handler?: string;            // Handler identifier
}

interface IntentParam {
  type: string;                // e.g., 'string', 'number', 'boolean'
  required?: boolean;
  description?: string;
  default?: unknown;
}
\`\`\`
`.trim();

// =============================================================================
// Framework-specific guidance
// =============================================================================

const NEXTJS_GUIDANCE = `
### Next.js App Router Integration

For route awareness, use Next.js navigation hooks:
\`\`\`tsx
import { usePathname, useParams, useSearchParams } from 'next/navigation';

// In a client component inside the layout:
const pathname = usePathname();
const params = useParams();
const searchParams = useSearchParams();

useRouteAwareness({
  params: params as Record<string, string>,
  queryParams: Object.fromEntries(searchParams),
});
\`\`\`

For page context, derive the page name from the pathname:
\`\`\`tsx
const pathname = usePathname();

// Map routes to semantic names
const pageInfo = useMemo(() => {
  if (pathname === '/') return { name: 'Home', section: 'main' };
  if (pathname.startsWith('/dashboard')) return { name: 'Dashboard', section: 'dashboard' };
  // ... more routes
  return { name: pathname, section: 'unknown' };
}, [pathname]);

usePageContext(pageInfo);
\`\`\`

**Important:** The UIBridgeHooks component must be a \`'use client'\` component placed inside the UIBridgeProvider in the root layout.
`.trim();

const REACT_ROUTER_GUIDANCE = `
### React Router Integration

For route awareness, use React Router hooks:
\`\`\`tsx
import { useLocation, useParams, useMatches } from 'react-router-dom';

const location = useLocation();
const params = useParams();
const matches = useMatches();

useRouteAwareness({
  pattern: matches[matches.length - 1]?.pathname,
  params: params as Record<string, string>,
  queryParams: Object.fromEntries(new URLSearchParams(location.search)),
  routeStack: matches.map(m => m.pathname),
});
\`\`\`

For page context, derive from the current route:
\`\`\`tsx
const location = useLocation();

const pageInfo = useMemo(() => {
  if (location.pathname === '/') return { name: 'Home', section: 'main' };
  // ... map routes to semantic names
  return { name: location.pathname, section: 'unknown' };
}, [location.pathname]);

usePageContext(pageInfo);
\`\`\`
`.trim();

// =============================================================================
// Category-specific prompt sections
// =============================================================================

const CATEGORY_INSTRUCTIONS: Record<HookCategory, string> = {
  "route-awareness": `
### Route Awareness (\`useRouteAwareness\`)

Look for:
- Router configuration files (route definitions, page directories)
- Route patterns with dynamic segments (e.g., \`/tasks/:id\`, \`/[slug]\`)
- Query parameter usage
- Nested routes and route stacks

Generate: A single \`useRouteAwareness()\` call that provides the full route info from the framework's router hooks.
`.trim(),

  "page-context": `
### Page Context (\`usePageContext\`)

Look for:
- All page/route components and their semantic purpose
- Page titles set via \`document.title\`, \`<title>\`, or \`Head\` components
- Breadcrumb components or navigation breadcrumb data
- Logical sections of the application (admin, settings, dashboard, etc.)

Generate: A \`usePageContext()\` call that derives the page name, section, and breadcrumb from the current route. Use a \`useMemo\` with a route-to-name mapping based on actual routes found in the code.
`.trim(),

  "state-machine": `
### State Machine (\`useUIState\`, \`useUITransition\`, \`useUIStateGroup\`)

Look for:
- Modals, dialogs, drawers, panels that open/close
- Tab groups and tab selection state
- Sidebar expand/collapse
- Authentication states (logged-in, logged-out, loading)
- Form wizard steps
- Dropdown menus
- Boolean state variables that control visibility (\`isOpen\`, \`showPanel\`, \`isExpanded\`)
- Conditional rendering based on state

For each detected UI state:
- Create a \`useUIState()\` call with a meaningful ID and name
- Set \`activeWhen\` to a function that checks the actual state variable
- Set \`blocking: true\` for modals/dialogs
- Set \`group\` for related states (e.g., all tabs in a tab group)
- List element IDs that belong to this state in \`elements\`

For transitions between states:
- Create \`useUITransition()\` calls connecting related states
- E.g., "open-settings-modal" transitions from "dashboard" to "settings-modal"

For tab groups, wizard steps, or exclusive states:
- Create a \`useUIStateGroup()\` with \`exclusive: true\`
`.trim(),

  components: `
### Component Actions (\`useUIComponent\`)

Look for:
- Components with significant actions (submit, delete, save, export, etc.)
- Components that expose state (form values, selection state, filter state)
- Components with computed properties (totals, filtered counts, validation status)
- Reusable widget components that have meaningful programmatic interfaces

Generate: \`useUIComponent()\` calls in or near the relevant component, with:
- \`actions\` containing handlers for the component's key actions
- \`state\` returning the component's current state
- \`computed\` for derived values
- \`elementIds\` listing the component's child element IDs
`.trim(),

  "keyboard-shortcuts": `
### Keyboard Shortcuts (\`useKeyboardShortcuts\`)

Look for:
- \`addEventListener('keydown', ...)\` handlers
- Keyboard event handlers in \`onKeyDown\` props
- Hotkey libraries (react-hotkeys-hook, mousetrap, etc.)
- Ctrl/Cmd+key combinations in event handlers
- Keyboard navigation implementations

Generate: A \`useKeyboardShortcuts()\` call registering all non-standard shortcuts found. Standard browser shortcuts (Ctrl+C, Ctrl+V) should NOT be included. Include:
- The key combo (e.g., "Ctrl+S", "Ctrl+Shift+N")
- A human-readable description
- The scope (e.g., "global", "editor", "modal")
`.trim(),

  "undo-redo": `
### Undo/Redo (\`useUndoRedo\`)

Look for:
- Custom undo/redo implementations in state management (Redux, Zustand, etc.)
- History stacks or history arrays in state
- \`undo()\` / \`redo()\` functions in hooks or stores
- \`canUndo\` / \`canRedo\` boolean state

Generate: A \`useUndoRedo()\` call wired to the app's undo system:
- \`canUndo\` / \`canRedo\` from the state
- \`onUndo\` / \`onRedo\` calling the app's undo/redo functions
- \`undoDescription\` from the undo stack if available

**Only generate this if the app has an actual undo/redo system.** Do not generate a stub.
`.trim(),

  annotations: `
### Element Annotations (\`useUIAnnotation\`)

Look for:
- Key interactive elements that would benefit from semantic descriptions
- Complex components where the DOM alone doesn't convey purpose
- Form fields with non-obvious validation or behavior
- Navigation elements with specific roles

Generate: \`useUIAnnotation()\` calls for important elements, providing:
- \`description\`: What the element is
- \`purpose\`: Why it exists and what it does
- \`tags\`: Searchable categories
- \`relatedElements\`: IDs of related UI elements

**Be selective.** Only annotate elements where the annotation adds value beyond what the DOM already communicates. Don't annotate every element.
`.trim(),

  intents: `
### Intents (\`registerIntent\`)

Look for:
- Multi-step user workflows (create project, submit form, onboard user)
- Actions that span multiple components or pages
- Common user goals that involve several clicks/interactions
- CRUD operations with specific steps

Generate: An \`intents.ts\` file with intent definitions. Each intent should have:
- A clear, descriptive \`id\` (e.g., "create-new-project")
- A human-readable \`name\`
- A detailed \`description\` of the steps involved
- \`params\` for any inputs the intent accepts
- \`tags\` for discoverability

Intents are registered via the API at startup, not as React hooks.
`.trim(),
};

// =============================================================================
// Output format instructions
// =============================================================================

const OUTPUT_FORMAT = `
## Output Format

**CRITICAL: Do NOT use file-writing tools (Write, Edit, etc.) to create or modify files.
Your output will be parsed by a script that extracts code blocks. You MUST output
all generated files as fenced code blocks in your text response.**

Output each generated file as a fenced code block with \`// FILE: <relative-path>\` on the **first line** inside the fence. Use \`tsx\` or \`ts\` as the language tag.

Example:
\`\`\`tsx
// FILE: src/lib/ui-bridge/UIBridgeHooks.tsx
'use client';

import { useMemo } from 'react';
import { usePathname, useParams, useSearchParams } from 'next/navigation';
import {
  useRouteAwareness,
  usePageContext,
  useUIState,
  useUITransition,
} from '@qontinui/ui-bridge/react';

export function UIBridgeHooks() {
  // ... hook calls here
  return null;
}
\`\`\`

\`\`\`ts
// FILE: src/lib/ui-bridge/intents.ts
import type { Intent } from '@qontinui/ui-bridge';

export const APP_INTENTS: Intent[] = [
  // ... intent definitions
];
\`\`\`

**If you do not output code blocks with \`// FILE:\` markers, the generation will fail.**
You may use file-reading tools to explore the codebase, but all generated code must be
output as text in the format above.

### File Structure Rules

1. **\`src/lib/ui-bridge/UIBridgeHooks.tsx\`** — Single \`'use client'\` component containing all global hooks:
   - \`useRouteAwareness()\` with framework-specific router integration
   - \`usePageContext()\` deriving name/section from current route
   - \`useUIState()\` calls for each detected UI state
   - \`useUITransition()\` calls connecting states
   - \`useUIStateGroup()\` for tab groups and exclusive state sets
   - \`useKeyboardShortcuts()\` with discovered shortcuts
   - \`useUndoRedo()\` wired to the app's state management (if applicable)
   - Renders \`null\` — placed inside the UIBridgeProvider tree

2. **\`src/lib/ui-bridge/intents.ts\`** — Intent definitions (only if intents category selected)

3. Per-component files only when \`useUIComponent()\` or \`useUIAnnotation()\` must be colocated with a specific component.
`.trim();

// =============================================================================
// Quality rules
// =============================================================================

const QUALITY_RULES = `
## Quality Rules

1. **Only generate hooks for things found in actual source code.** Never guess or fabricate.
2. **Use real component names, route paths, element IDs, and state variable names.**
3. Every \`useUIState\` must have a real \`activeWhen\` condition referencing actual app state.
4. Every \`useUITransition\` must connect real states found in the app.
5. Every \`useKeyboardShortcuts\` entry must correspond to a real keyboard handler in the code.
6. Do NOT generate \`useUndoRedo\` unless the app has an actual undo/redo system.
7. Import hooks from \`'@qontinui/ui-bridge/react'\` (not from internal paths).
8. Import types from \`'@qontinui/ui-bridge'\`.
9. The UIBridgeHooks component must render \`null\` — it's a hook container, not a visual component.
10. Use \`useMemo\` for derived data (e.g., route-to-page-name mapping) to avoid unnecessary re-renders.
`.trim();

// =============================================================================
// Exploration instructions
// =============================================================================

const EXPLORATION_INSTRUCTIONS = `
## CRITICAL: Read the Source Code First

You MUST explore the actual source code before generating any hooks. Do not guess at app structure.

**IMPORTANT: Use file-reading tools to explore the code, but do NOT use file-writing
tools (Write, Edit, etc.) to create or modify any files. Your final output must be
code blocks in your text response with \`// FILE:\` markers — see the Output Format section.**

### Code exploration steps:
1. **Find the router configuration** — look for route definitions, page directories, router setup files
2. **Read page/layout components** — understand the component tree, what wraps what
3. **Identify UI states** — find modals, drawers, tabs, panels, sidebars and their state management
4. **Find keyboard handlers** — search for keydown listeners, hotkey libraries
5. **Check for undo/redo** — look in state management (stores, reducers, context) for history/undo patterns
6. **Identify major components** — find components with significant actions or state
7. **Look for multi-step flows** — user journeys that span multiple interactions

### Progress communication
Output brief progress notes as you explore. After each major discovery, write 1-2 sentences:
- "Found 12 routes in app/router.tsx, including /dashboard, /settings, /projects/:id..."
- "Detected sidebar toggle (isCollapsed state in useSidebar hook) and 3 modals..."
- "No undo/redo system found — skipping useUndoRedo."
`.trim();

// =============================================================================
// Public API
// =============================================================================

export interface ProjectAnalysisForHooks {
  framework: string;
  project_path: string;
}

/**
 * Build the AI prompt for generating UI Bridge hooks from scratch.
 */
export function buildHookGenPrompt(
  analysis: ProjectAnalysisForHooks,
  categories: HookCategory[],
): string {
  const lines: string[] = [];

  lines.push("# UI Bridge Hook Generation");
  lines.push("");
  lines.push("You are generating UI Bridge hook integration code for a real application.");
  lines.push("Your job is to read the application's source code, understand its structure,");
  lines.push("and generate accurate, app-specific hook files that register the app's UI states,");
  lines.push("routes, components, shortcuts, and intents with the UI Bridge.");
  lines.push("");
  lines.push("**SYSTEM CONSTRAINT: You MUST NOT use any file-writing tools (Write, Edit,");
  lines.push("save_file, create_file, etc.). Your output is parsed by a script. All generated");
  lines.push(
    "code must appear as fenced code blocks in your text response with `// FILE:` markers.",
  );
  lines.push(
    "If you write files directly instead of outputting code blocks, the generation will fail.**",
  );
  lines.push("");

  // Exploration instructions
  lines.push(EXPLORATION_INSTRUCTIONS);
  lines.push("");

  // Framework-specific guidance
  const fw = analysis.framework.toLowerCase();
  if (fw === "nextjs" || fw === "next_js") {
    lines.push(NEXTJS_GUIDANCE);
  } else if (fw === "react") {
    lines.push(REACT_ROUTER_GUIDANCE);
  }
  lines.push("");

  // Hook API reference
  lines.push(HOOK_API_REFERENCE);
  lines.push("");

  // Category-specific instructions (only for selected categories)
  lines.push("## Categories to Generate");
  lines.push("");
  for (const cat of categories) {
    lines.push(CATEGORY_INSTRUCTIONS[cat]);
    lines.push("");
  }

  // Output format
  lines.push(OUTPUT_FORMAT);
  lines.push("");

  // Quality rules
  lines.push(QUALITY_RULES);

  return lines.join("\n");
}

/**
 * Build the AI prompt for regenerating hooks when they already exist.
 * Includes existing hook file contents for the AI to update/preserve.
 */
export function buildHookRegenPrompt(
  analysis: ProjectAnalysisForHooks,
  categories: HookCategory[],
  existingCode: string,
): string {
  const basePrompt = buildHookGenPrompt(analysis, categories);

  const regenSection = `
## Regeneration Mode — Updating Existing Hooks

You are UPDATING existing hook files, not creating from scratch. The current code is below.

**IMPORTANT: Do NOT edit the existing files directly with tools. Output the COMPLETE
updated file contents as a code block with the \`// FILE:\` marker, just like fresh
generation. The system will apply the changes for you.**

### Merge Rules:
1. **Read the source code first** (same process as fresh generation)
2. **Compare existing hooks against what you find in the code:**
   - If an existing hook covers functionality that still exists → KEEP it, enhance if you can add detail
   - If an existing hook covers functionality that was removed → REMOVE it
   - If you find functionality not covered by any existing hook → ADD a new hook
3. **NEVER reduce detail** — if an existing hook has detailed configuration, your replacement must be at least as detailed
4. **Preserve working code** — if something works correctly, don't change it unnecessarily
5. **Add hooks for new features** — if new routes, modals, or components have been added, include them

### Existing Code:

\`\`\`tsx
${existingCode}
\`\`\`
`.trim();

  return basePrompt + "\n\n---\n\n" + regenSection;
}

// =============================================================================
// Architecture Spec Prompt (delegates to spec-prompt-builder)
// =============================================================================

import { SPEC_CREATION_INSTRUCTIONS, SPEC_MERGE_INSTRUCTIONS } from "./spec-prompt-builder";

/**
 * Build an AI prompt for generating an architecture spec for the project.
 * Delegates to the canonical SPEC_CREATION_INSTRUCTIONS and
 * appends framework context.
 */
export function buildArchitectureSpecPrompt(analysis: ProjectAnalysisForHooks): string {
  return `${SPEC_CREATION_INSTRUCTIONS}

---

Create a comprehensive architecture spec for this project. Read the source code first, then generate a complete JSON spec.

Framework detected: **${analysis.framework}**
Project path: \`${analysis.project_path}\``;
}

/**
 * Build an AI prompt for regenerating/updating an existing architecture spec.
 */
export function buildArchitectureSpecRegenPrompt(
  analysis: ProjectAnalysisForHooks,
  existingSpec: string,
): string {
  return `${SPEC_CREATION_INSTRUCTIONS}

---

${SPEC_MERGE_INSTRUCTIONS}

---

## Current Architecture Spec to Merge With

### Raw JSON (preserve feature IDs and enhance detail)

\`\`\`json
${existingSpec}
\`\`\`

---

Update this architecture spec by reading the current source code and merging improvements.

Framework detected: **${analysis.framework}**
Project path: \`${analysis.project_path}\``;
}
