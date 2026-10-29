/**
 * hook-gen-prompt-builder.ts
 *
 * Builds AI prompts for generating UI Bridge hook integration code.
 * The AI reads the project's source code and generates app-specific
 * hook files for route awareness, page context, state machine, etc.
 */

import { isReactNativeFramework } from "./page-analysis-prompt-builder";

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

/**
 * The `usePageContext` entry of the API reference — WEB ONLY. The native SDK
 * exports no `usePageContext` and its snapshot carries no page name (see
 * `RN_NO_PAGE_CONTEXT`), so a React Native prompt never shows it.
 */
const PAGE_CONTEXT_API_REFERENCE = `
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

**\`name\` is never derived from the pathname.** The runner slugs \`name\` into
the journey ledger's page label, so a name taken from the pathname on \`/search/<what the user
typed>\` stores \`search-<what the user typed>\`. Map each route you found to a
FIXED name, and give every route you did not map the fixed fallback
\`{ name: 'Unknown', section: 'unknown' }\` — never the pathname, raw or
transformed.

`;

/**
 * The hook API reference of a React Native prompt: ONLY what
 * `@qontinui/ui-bridge-native` exports (ui-bridge d84bfa6
 * `packages/ui-bridge-native/src/index.ts`), with the native types —
 * `useRouteAwareness`'s `unmatched` is a plain boolean there
 * (`src/react/useRouteAwareness.ts`), not the web `RouteUnmatchedSignal`.
 */
const RN_HOOK_API_REFERENCE = `
## UI Bridge Native API Reference (\`@qontinui/ui-bridge-native\`)

Import every hook and component below from \`@qontinui/ui-bridge-native\`, and
only from there. It does NOT export \`useUIState\`, \`useUITransition\`, \`useUIStateGroup\`, \`useKeyboardShortcuts\`, \`useUndoRedo\`, \`useUIAnnotation\`, \`usePageContext\`, \`routePatternFromParams\`, \`useRouteUnmatchedSignal\`, \`useMarkRouteUnmatched\` or \`RouteUnmatchedContext\` — those are web APIs.

### useRouteAwareness(info: RouteInfo, options?: { unmatched?: boolean }): void
Reports the router's route TEMPLATE as the snapshot's \`page.route\`. Cleared
when the component unmounts; a no-op outside \`UIBridgeNativeProvider\`.

\`\`\`typescript
interface RouteInfo {
  pattern?: string | null;       // Route TEMPLATE ("/runner/[id]"), never the pathname
  patternSource?: 'router';      // Asserts \`pattern\` came from the router
  params?: Record<string, string>;
  queryParams?: Record<string, string>;
  routeStack?: string[];         // One cumulative template per depth, root first
}
\`\`\`

Derive \`pattern\` with \`expoRouterPattern(useSegments())\` (Expo Router) or
\`reactNavigationPattern(rootState)\` (React Navigation), and \`routeStack\` with
\`expoRouterRouteStack(segments)\`. Each returns \`null\` / \`undefined\` rather than
leak a concrete path. Pass \`{ unmatched: isExpoRouterUnmatched(segments) }\`:
while it is true the hook reports \`pattern: null\` and drops \`params\`.

### useUIElement(options): { ref, onLayout, bridgeProps, registered, getState, getIdentifier }
Registers one interactive element. Spread \`bridgeProps\` onto the host
component together with \`ref\` and \`onLayout\`. The options type is a union
over \`type\`: for \`'button'\` and \`'pressable'\`, \`handlers.onPress\` is
REQUIRED (UI Bridge invokes it on a \`press\` action); for every other type
\`handlers\` is optional.

\`\`\`typescript
interface UseUIElementOptionsBase<T extends NativeElementType = NativeElementType> {
  id: string;                    // Stable, unique
  type?: T;                      // 'button' | 'input' | ... (default 'custom')
  label?: string;                // What the element is, for an agent
  value?: string;                // A text input's controlled value -> state.value
  actions?: NativeStandardAction[];
  customActions?: Record<string, NativeCustomAction>;
}
interface UseUIElementOptionsHandlersRequired<
  T extends PressNeedingNativeElementType = PressNeedingNativeElementType, // 'button' | 'pressable'
> extends UseUIElementOptionsBase<T> {
  handlers: { onPress: PressHandler; [key: string]: ((...args: any[]) => any) | undefined };
}
interface UseUIElementOptionsHandlersOptional<
  T extends NativeElementType = NativeElementType,
> extends UseUIElementOptionsBase<T> {
  handlers?: Record<string, ((...args: any[]) => any) | undefined>;
}
type UseUIElementOptions<T extends NativeElementType = NativeElementType> =
  T extends PressNeedingNativeElementType
    ? UseUIElementOptionsHandlersRequired<T>
    : UseUIElementOptionsHandlersOptional<T>;
\`\`\`

### useUIComponent(options: UseUIComponentOptions): { registered, executeAction }
Registers a screen-level action surface.

\`\`\`typescript
interface UseUIComponentOptions {
  id: string;
  name: string;
  description?: string;
  actions?: ComponentActionDef[]; // { id, label?, description?, paramSchema?, effect?, handler }
  elementIds?: string[];
  autoRegister?: boolean;
}
\`\`\`

### State / TransitionTo (IR annotation components)
Runtime no-ops that render their children; the build-time IR extractor
(\`@qontinui/ui-bridge-auto\`) reads the JSX to derive the screen's states and
transitions.

\`\`\`typescript
interface StateProps {
  id: string;
  name?: string;
  description?: string;
  requiredElements?: { role?: string; text?: string; testID?: string }[];
  blocking?: boolean;
}
interface TransitionToProps {
  id: string;
  name?: string;
  fromStates?: string[];
  activateStates?: string[];
  exitStates?: string[];
  effect?: string;
  description?: string;
}
\`\`\`

### UndoTracker (from the provider context)
\`useUIBridgeNative().undoTracker.setDeclaredState(state: DeclaredUndoState)\`
declares the app's undo state; snapshots then carry it as \`undoRedo\`.

\`\`\`typescript
interface DeclaredUndoState {
  canUndo: boolean;
  canRedo: boolean;
  undoDescription?: string;
  redoDescription?: string;
  undoDepth?: number;
  redoDepth?: number;
}
\`\`\`
`.trim();

/** The hook API reference for a web (`false`) or React Native (`true`) prompt. */
function hookApiReference(isRN: boolean): string {
  return isRN
    ? RN_HOOK_API_REFERENCE
    : hookApiReferenceWith(PAGE_CONTEXT_API_REFERENCE.trimStart());
}

const hookApiReferenceWith = (PAGE_CONTEXT_API_SLOT: string): string =>
  `
## UI Bridge Hook API Reference

### useRouteAwareness(info: RouteInfo, options?: { unmatched?: RouteUnmatchedSignal | null }): void
Provides framework-router integration for the navigation tracker.

\`\`\`typescript
interface RouteInfo {
  pattern?: string | null;       // Route PATTERN (e.g., "/tasks/[id]"), never the URL path
  patternSource?: 'router';      // Asserts \`pattern\` came from the router
  params?: Record<string, string>;  // Extracted route parameters
  queryParams?: Record<string, string>;  // Query parameters
  routeStack?: string[];         // Matched route stack / breadcrumb (route ids)
}
\`\`\`

**\`pattern\` is never the pathname.** A concrete path such as \`/search/<what the
user typed>\` carries user input, and the runner's journey ledger stores
\`pattern\` ONLY when \`patternSource: 'router'\` asserts it came from the router.
Derive it with \`routePatternFromParams(pathname, params, { matched })\` from
\`@qontinui/ui-bridge/react\`: it puts every param value back in its \`[name]\`
slot and returns \`null\` when the route is unmatched (\`matched: false\`, e.g. a
404) or a value cannot be substituted. Pass the router's params RAW — never
pre-join a catch-all array. If the installed SDK predates the helper, OMIT
\`pattern\` (and \`patternSource\`) entirely; never fall back to the pathname.

**A 404 needs the not-found signal.** On a 404 the router's params are \`{}\`,
so \`routePatternFromParams(pathname, {}, { matched: true })\` returns the
CONCRETE path. A layout cannot know it is rendering a 404, so the SDK's
provider-owned signal does it: the layout creates
\`const unmatched = useRouteUnmatchedSignal()\`, provides it through
\`<RouteUnmatchedContext.Provider value={unmatched}>\`, and passes it to its ONE
call, \`useRouteAwareness(info, { unmatched })\`; the not-found boundary calls
\`useMarkRouteUnmatched()\` and never calls \`useRouteAwareness\` itself. While
it is mounted the hook reports \`pattern: null\`.

${PAGE_CONTEXT_API_SLOT}### useUIState(options: UseUIStateOptions): UseUIStateReturn
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
`;

// =============================================================================
// Framework-specific guidance
// =============================================================================

const NEXTJS_GUIDANCE = `
### Next.js App Router Integration

For route awareness, mount ONE component in the root layout, and mark the
not-found page:
\`\`\`tsx
// app/RouteAwareness.tsx — mounted once, in the root layout
'use client';
import { usePathname, useParams, useSearchParams } from 'next/navigation';
import {
  RouteUnmatchedContext,
  routePatternFromParams,
  useRouteAwareness,
  useRouteUnmatchedSignal,
} from '@qontinui/ui-bridge/react';

export function RouteAwareness({ children }: { children: React.ReactNode }) {
  const pathname = usePathname();
  const params = useParams(); // RAW: a catch-all is a string[] — do not join it
  const searchParams = useSearchParams();
  const unmatched = useRouteUnmatchedSignal();

  useRouteAwareness(
    {
      // "/search/abc" with { term: "abc" } -> "/search/[term]".
      // matched: true is safe ONLY because \`unmatched\` overrides it on a 404.
      pattern: routePatternFromParams(pathname, params, { matched: true }),
      patternSource: 'router',
      queryParams: Object.fromEntries(searchParams),
    },
    { unmatched },
  );

  return (
    <RouteUnmatchedContext.Provider value={unmatched}>{children}</RouteUnmatchedContext.Provider>
  );
}
\`\`\`

\`\`\`tsx
// app/not-found.tsx — REQUIRED. Do NOT call useRouteAwareness here.
'use client';
import { useMarkRouteUnmatched } from '@qontinui/ui-bridge/react';

export default function NotFound() {
  useMarkRouteUnmatched();
  return <p>Not found</p>;
}
\`\`\`

Without \`not-found.tsx\` marking the signal, a 404 reports its concrete path
as a router pattern. Never write \`pattern: pathname\` — the pathname is user
input, not a pattern.

For page context, map each known route to a FIXED semantic name:
\`\`\`tsx
const pathname = usePathname();

// Map routes to semantic names
const pageInfo = useMemo(() => {
  if (pathname === '/') return { name: 'Home', section: 'main' };
  if (pathname.startsWith('/dashboard')) return { name: 'Dashboard', section: 'dashboard' };
  // ... more routes
  // Unmapped: a FIXED name. Never the pathname — it carries user input.
  return { name: 'Unknown', section: 'unknown' };
}, [pathname]);

usePageContext(pageInfo);
\`\`\`

**Important:** The UIBridgeHooks component must be a \`'use client'\` component placed inside the UIBridgeProvider in the root layout.
`.trim();

const REACT_ROUTER_GUIDANCE = `
### React Router Integration

For route awareness, host the hook in a layout route's element (so
\`useParams()\` returns the matched route's params) and give the router a
\`path="*"\` route:
\`\`\`tsx
import { useLocation, useParams, Outlet } from 'react-router-dom';
import {
  RouteUnmatchedContext,
  routePatternFromParams,
  useMarkRouteUnmatched,
  useRouteAwareness,
  useRouteUnmatchedSignal,
} from '@qontinui/ui-bridge/react';

function AppLayout() {
  const location = useLocation();
  const params = useParams();
  const unmatched = useRouteUnmatchedSignal();

  useRouteAwareness(
    {
      pattern: routePatternFromParams(location.pathname, params, { matched: true }),
      patternSource: 'router',
      queryParams: Object.fromEntries(new URLSearchParams(location.search)),
    },
    { unmatched },
  );

  return (
    <RouteUnmatchedContext.Provider value={unmatched}>
      <Outlet />
    </RouteUnmatchedContext.Provider>
  );
}

// The element of the REQUIRED \`path="*"\` route, below the provider.
function NoMatch() {
  useMarkRouteUnmatched();
  return <p>Not found</p>;
}
\`\`\`

**A \`path="*"\` route is REQUIRED.** Without one, an unmatched URL renders the
layout with \`params = {}\`, and the concrete path leaks as the pattern. With
one, the \`*\` splat matches and templates the path as \`[...*]\`; its element
marking the signal reports \`null\` instead. \`useMatches()\` is NOT a 404 test:
in a data router an unmatched URL still yields \`[root]\`. If the root route's
\`errorElement\` hosts the hook itself, it owns the signal and must call
\`useMarkRouteUnmatched(is404 ? unmatched : null)\` with it explicitly.

Never write \`pattern: location.pathname\` or a match's \`pathname\` — both are
the URL the user is on, not a pattern.

For page context, map each known route to a FIXED semantic name:
\`\`\`tsx
const location = useLocation();

const pageInfo = useMemo(() => {
  if (location.pathname === '/') return { name: 'Home', section: 'main' };
  // ... map routes to semantic names
  // Unmapped: a FIXED name. Never the pathname — it carries user input.
  return { name: 'Unknown', section: 'unknown' };
}, [location.pathname]);

usePageContext(pageInfo);
\`\`\`
`.trim();

/**
 * The Expo Router route-awareness recipe, shared by the integration guidance
 * and the RN route-awareness category. It teaches the SDK's own helpers
 * (`@qontinui/ui-bridge-native` `react/routePattern.ts`): they derive the
 * template from `useSegments()` and fail closed to `null`, so the generated
 * code never builds a pattern by hand and never touches the pathname.
 * The hook and helpers first shipped together in ui-bridge-native 0.6.16
 * (ui-bridge 9e5515f / bbe7d7a).
 */
const EXPO_ROUTE_AWARENESS = `
\`\`\`tsx
// app/_layout.tsx — mount <RouteAwareness /> once, inside UIBridgeNativeProvider
import { useSegments } from 'expo-router';
import {
  expoRouterPattern,
  expoRouterRouteStack,
  isExpoRouterUnmatched,
  useRouteAwareness,
} from '@qontinui/ui-bridge-native';

function RouteAwareness() {
  const segments = useSegments(); // the FILE route: ["runner", "[id]"], never "42"
  useRouteAwareness(
    {
      // "/runner/[id]"; null on +not-found or a segment that is not a route name.
      pattern: expoRouterPattern(segments),
      patternSource: 'router',
      routeStack: expoRouterRouteStack(segments),
    },
    // +not-found: the hook reports pattern: null and drops params.
    { unmatched: isExpoRouterUnmatched(segments) },
  );
  return null;
}
\`\`\`

Use the SDK helpers; do not build the pattern by hand. Never write
\`pattern: pathname\` or \`pattern: usePathname()\` — the pathname is user input,
not a pattern — and do not pass \`useLocalSearchParams()\` as \`params\` from the
root layout. Do NOT import \`@qontinui/ui-bridge/react\` in a React Native app: it
is the DOM barrel, and \`@qontinui/ui-bridge\` is only an optional peer of
\`@qontinui/ui-bridge-native\`. \`useRouteAwareness\`, \`expoRouterPattern\`,
\`expoRouterRouteStack\` and \`isExpoRouterUnmatched\` are all exported by
\`@qontinui/ui-bridge-native\` 0.6.16 and later. If the installed version is
older, upgrade it; if you cannot, skip route awareness and say so rather than
hand-building a pattern or importing it from elsewhere.
`.trim();

/**
 * Why the RN prompt teaches NO page context: `@qontinui/ui-bridge-native`
 * exports no `usePageContext` (only the DOM barrel, `@qontinui/ui-bridge/react`,
 * does), and the native snapshot's `page` is `NativeSnapshotPageContext`,
 * which carries `route` only (ui-bridge `packages/ui-bridge-native/src/core/types.ts`).
 * A page name would have nowhere to go, so the route template reported through
 * `useRouteAwareness` is the screen's identity.
 */
const RN_NO_PAGE_CONTEXT = `
**No page context on React Native.** \`@qontinui/ui-bridge-native\` does NOT
export \`usePageContext\`, and a native snapshot's \`page\` carries route info only
(\`route\`, reported through \`useRouteAwareness\`) — there is no page name,
section or breadcrumb in it. Identify screens through route awareness: the
route template (\`/connect\`, \`/(tabs)/prompts\`, \`/runner/[id]\`) IS the screen's
identity. Generate no page-context call, and import nothing from
\`@qontinui/ui-bridge/react\` (the DOM barrel).
`.trim();

const RN_EXPO_GUIDANCE = `
### React Native + Expo Router Integration

This is a React Native / Expo Router project. **DOM APIs are not available** —
no \`document\`, no \`window.addEventListener\`, no \`getBoundingClientRect\`.
Element measurement is asynchronous via \`measureInWindow\`. The native SDK
(\`@qontinui/ui-bridge-native\`) handles measurement for you through the
\`bridgeProps\` object returned by \`useUIElement\`; the integrator just spreads
it onto the host component along with \`ref\` and \`onLayout\`.

For route awareness, use Expo Router hooks (NOT \`react-router-dom\`, NOT
\`next/navigation\`):

${EXPO_ROUTE_AWARENESS}

${RN_NO_PAGE_CONTEXT}

**Modal-presentation \`Stack.Screen\`s are still pages** — their route template
identifies them, exactly as for normal screens.

**Imports:** Use \`@qontinui/ui-bridge-native\` (NOT \`@qontinui/ui-bridge/react\`)
for all hooks in an Expo / React Native project, and its types from
\`@qontinui/ui-bridge-native\` too. Only the \`Intent\` type comes from
\`@qontinui/ui-bridge\` — an optional peer the app must add to its own
dependencies before importing from it.
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
// React Native / Expo Router category overrides
// =============================================================================
//
// These replace (not append to) the web-flavored category instructions when
// the project is detected as `react_native` or `expo_router`. The web text
// references DOM-only constructs (`document.title`, `addEventListener`,
// `getBoundingClientRect`) and component patterns (`<input>`, `<form>`) that
// don't exist in React Native, so a straight append would actively mislead
// the AI. Each branch below mirrors the structure of its web counterpart and
// includes at least one concrete code snippet so the AI has a worked example
// to follow.

const RN_CATEGORY_INSTRUCTIONS: Record<HookCategory, string> = {
  "route-awareness": `
### Route Awareness (\`useRouteAwareness\`) — React Native / Expo Router

Look for:
- The \`app/\` directory structure (Expo Router uses file-based routing)
- Route group folders like \`app/(tabs)/\`, \`app/(auth)/\`
- Dynamic segment files like \`app/[id].tsx\`, \`app/posts/[slug].tsx\`
- \`<Stack.Screen>\` and \`<Tabs.Screen>\` declarations
- Existing usage of \`usePathname\`, \`useSegments\`, \`useLocalSearchParams\`

Generate: A single \`useRouteAwareness()\` call in a top-level component
(typically \`app/_layout.tsx\`, inside \`UIBridgeNativeProvider\`) that
provides the full route info from \`expo-router\` hooks.

${EXPO_ROUTE_AWARENESS}

**Do NOT use** \`useLocation()\` (that's react-router-dom) or
\`next/navigation\` hooks — they don't exist in Expo.
`.trim(),

  "page-context": `
### Page Context — React Native / Expo Router (not available)

${RN_NO_PAGE_CONTEXT}

Generate NOTHING for this category, and say so in your response: page
identity on React Native comes from the Route Awareness category.
`.trim(),

  "state-machine": `
### State Machine (\`State\`, \`TransitionTo\`) — React Native / Expo Router

\`@qontinui/ui-bridge-native\` does NOT export \`useUIState\`, \`useUITransition\` or \`useUIStateGroup\` (those are web hooks). Its state vocabulary is the pair
of IR annotation components \`State\` and \`TransitionTo\`: runtime no-ops that
render their children and are read at BUILD time by the IR extractor
(\`@qontinui/ui-bridge-auto\`). There is no runtime \`activeWhen\`: the state is
the subtree the component wraps.

Look for:
- \`<Modal visible={…}>\` from \`react-native\` or any third-party modal lib
- Bottom-sheet / drawer libraries (\`@gorhom/bottom-sheet\`, \`react-native-modal\`)
- \`<Tabs>\` / \`<Tabs.Screen>\` from Expo Router (each tab is a page, not a state —
  route awareness identifies those)
- Authentication states (logged-in vs logged-out gates around \`<Slot />\` or
  \`<Stack />\` in \`app/_layout.tsx\`)
- Boolean state variables controlling visibility (\`isOpen\`, \`showSheet\`,
  \`isExpanded\`) and conditional rendering driven by them

For each detected UI state, wrap the subtree that IS the state in \`<State>\`:
- \`id\`: meaningful and stable; \`name\` and \`description\` for an agent
- \`blocking\` for full-screen modals and bottom sheets that block the screen
- \`requiredElements\`: \`{ role, text, testID }\` entries the state must show

For each transition, wrap the control that triggers it in \`<TransitionTo>\`
with \`id\`, \`fromStates\`, \`activateStates\`, \`exitStates\` and, when the action's
effect is known, \`effect\`.

\`\`\`tsx
import { useState } from 'react';
import { Pressable, Text } from 'react-native';
import { State, TransitionTo } from '@qontinui/ui-bridge-native';

function PromptsList() {
  const [sheetOpen, setSheetOpen] = useState(false);
  return (
    <>
      <TransitionTo id="open-filter-sheet" fromStates={['prompts-list']} activateStates={['filter-sheet']}>
        <Pressable onPress={() => setSheetOpen(true)}>
          <Text>Filter</Text>
        </Pressable>
      </TransitionTo>
      {sheetOpen && (
        <State id="filter-sheet" name="Filter Bottom Sheet" blocking>
          <FilterSheet onClose={() => setSheetOpen(false)} />
        </State>
      )}
    </>
  );
}
\`\`\`

There is no state-group component: express mutually exclusive states (wizard
steps, radio-style sets) as separate \`State\`s whose transitions name the
others in \`exitStates\`.
`.trim(),

  components: `
### Component Actions (\`useUIElement\` and \`useUIComponent\`) — React Native / Expo Router

The native SDK uses **\`useUIElement\`** for individual interactive elements
and **\`useUIComponent\`** for screens that should expose a higher-level
action surface. Both come from \`@qontinui/ui-bridge-native\`.

**RN measurement gotcha:** there is no \`getBoundingClientRect\` on RN host
components. Layout is async and goes through \`measureInWindow\`. The SDK
handles this for you via the \`bridgeProps\` object — you just need to spread
\`bridgeProps\` onto the host component along with \`ref\` and \`onLayout\`. Do
NOT try to call \`measureInWindow\` yourself, and do NOT emit any web
measurement code (\`getBoundingClientRect\`, \`offsetTop\`, etc.).

#### Pressable / Touchable

Wrap with \`useUIElement({ type: 'button', label: '…', handlers: { onPress } })\` —
\`handlers.onPress\` is required for \`'button'\` and \`'pressable'\`. The keys to
attach are \`ref\`, \`onLayout\`, and the spread of \`bridgeProps\`:

\`\`\`tsx
import { Pressable, Text } from 'react-native';
import { useUIElement } from '@qontinui/ui-bridge-native';

function CreateButton({ onPress }: { onPress: () => void }) {
  const { ref, onLayout, bridgeProps } = useUIElement({
    id: 'prompts-create-button',
    type: 'button',
    label: 'Create prompt',
    handlers: { onPress },
  });
  return (
    <Pressable ref={ref} onLayout={onLayout} {...bridgeProps} onPress={onPress}>
      <Text>Create</Text>
    </Pressable>
  );
}
\`\`\`

#### TextInput

\`useUIElement({ type: 'input', label: '…', value })\`. Pass the same \`value\`
you bind to the \`<TextInput>\` so the registered element's \`state.value\` is what
the user sees; \`onChangeText\` stays on the \`<TextInput>\` itself, and
\`bridgeProps\` carries only the bridge metadata.

\`\`\`tsx
import { TextInput } from 'react-native';
import { useUIElement } from '@qontinui/ui-bridge-native';

function SearchInput({ value, onChange }: { value: string; onChange: (v: string) => void }) {
  const { ref, onLayout, bridgeProps } = useUIElement({
    id: 'prompts-search-input',
    type: 'input',
    label: 'Search prompts',
    value,
  });
  return (
    <TextInput
      ref={ref}
      onLayout={onLayout}
      {...bridgeProps}
      value={value}
      onChangeText={onChange}
      placeholder="Search…"
    />
  );
}
\`\`\`

#### FlatList items

Register each rendered item with a deterministic ID derived from the model
(e.g., the row's \`.id\` field) inside \`renderItem\`. Do NOT use the array
index — IDs must be stable across re-orders and pagination.

\`\`\`tsx
<FlatList
  data={prompts}
  keyExtractor={(p) => p.id}
  renderItem={({ item }) => <PromptRow id={\`prompt-row-\${item.id}\`} prompt={item} />}
/>
\`\`\`

…where \`PromptRow\` calls \`useUIElement\` with the passed-in \`id\`.

#### Prefer \`useUIComponent\` for whole screens

For screens with form state plus multiple actions, **prefer a single
\`useUIComponent\`** over many per-element registrations. The \`actions\` map
should expose semantic verbs (\`refresh\`, \`submit\`, \`select-item({id})\`)
that wrap the same handlers the buttons do, so an automation agent can
drive the screen with one HTTP call instead of fighting per-button IDs.

\`\`\`tsx
import { useState } from 'react';
import { useUIComponent } from '@qontinui/ui-bridge-native';

function PromptsScreen() {
  const [query, setQuery] = useState('');
  const { data, refetch } = usePrompts();

  useUIComponent({
    id: 'prompts-screen',
    name: 'Prompts',
    actions: [
      { id: 'refresh', label: 'Refresh', handler: () => refetch() },
      {
        id: 'select-item',
        label: 'Open prompt',
        // A handler receives \`params?: unknown\`: narrow it, never annotate it.
        handler: (params) => {
          const { id } = params as { id: string };
          router.push(\`/prompts/\${id}\`);
        },
      },
      {
        id: 'set-query',
        label: 'Set search query',
        handler: (params) => setQuery((params as { q: string }).q),
      },
    ],
  });

  // ...render UI
}
\`\`\`

Use \`useUIElement\` for things the screen-level component can't model (e.g.,
the heading text the test framework wants to assert on, or a single primary
CTA). Use \`useUIComponent\` for the screen's full action surface.
`.trim(),

  "keyboard-shortcuts": `
### Keyboard Shortcuts — React Native / Expo Router (not available)

\`@qontinui/ui-bridge-native\` does NOT export \`useKeyboardShortcuts\` or any other keyboard-shortcut API, and touch is the primary input on mobile.
Generate NOTHING for this category, and say so in your response. Do NOT emit
web event-listener code (\`addEventListener('keydown', ...)\`) — that's a no-op
in React Native.
`.trim(),

  "undo-redo": `
### Undo/Redo (\`UndoTracker\`) — React Native / Expo Router

\`@qontinui/ui-bridge-native\` does NOT export \`useUndoRedo\`. Its undo surface is
the provider's \`UndoTracker\`: read it from the context with
\`useUIBridgeNative()\` (inside \`UIBridgeNativeProvider\`) and declare the app's
undo state with \`undoTracker.setDeclaredState(state: DeclaredUndoState)\`;
snapshots then carry it as \`undoRedo\`.

Only generate this if the app has an actual undo/redo system. Look for:
- A history stack in a Redux / Zustand / Jotai / Recoil store
- An \`undo()\` / \`redo()\` function in a hook or store
- \`canUndo\` / \`canRedo\` boolean state
- Common RN libraries: \`zundo\` (Zustand middleware), \`redux-undo\`

\`\`\`tsx
import { useEffect } from 'react';
import { useUIBridgeNative } from '@qontinui/ui-bridge-native';

// canUndo / canRedo come from the app's own undo store.
function UndoStateReporter({ canUndo, canRedo }: { canUndo: boolean; canRedo: boolean }) {
  const { undoTracker } = useUIBridgeNative();
  useEffect(() => {
    undoTracker.setDeclaredState({ canUndo, canRedo });
  }, [undoTracker, canUndo, canRedo]);
  return null;
}
\`\`\`

**Do not generate a stub.**
`.trim(),

  annotations: `
### Element Annotations — React Native / Expo Router (not available)

\`@qontinui/ui-bridge-native\` does NOT export \`useUIAnnotation\` or any other annotation API.
Generate NOTHING for this category, and say so in your response: on React
Native an element's purpose is carried by the \`label\` you give \`useUIElement\`
and the \`name\` / \`description\` of a \`useUIComponent\` or \`State\`.
`.trim(),

  intents: `
### Intents — React Native / Expo Router

Same as the web guidance — intents describe named composite user actions,
not framework specifics. Look for:
- Multi-step user workflows that span screens (sign-up flow, create-prompt,
  configure-runner)
- Actions that span multiple navigation transitions
- Common user goals involving several taps/screens

Generate an \`intents.ts\` file with intent definitions. Each intent
should have:
- A clear, descriptive \`id\` (e.g., "create-new-prompt")
- A human-readable \`name\`
- A detailed \`description\` of the steps involved (mention the screens it
  navigates through)
- \`params\` for any inputs the intent accepts
- \`tags\` for discoverability

Intents are registered via the API at startup, not as React hooks:
\`@qontinui/ui-bridge-native\` has no intent hook, so generate the
\`intents.ts\` data file only. Its \`Intent\` type comes from \`@qontinui/ui-bridge\`:
\`@qontinui/ui-bridge\` is only an OPTIONAL peer of \`@qontinui/ui-bridge-native\`:
if the app emits intents or imports a type from it, add \`@qontinui/ui-bridge\`
to the app's own dependencies first.
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

/** The output format for a React Native / Expo prompt: native imports, no page context. */
const RN_OUTPUT_FORMAT = `
## Output Format

**CRITICAL: Do NOT use file-writing tools (Write, Edit, etc.) to create or modify files.
Your output will be parsed by a script that extracts code blocks. You MUST output
all generated files as fenced code blocks in your text response.**

Output each generated file as a fenced code block with \`// FILE: <relative-path>\` on the **first line** inside the fence. Use \`tsx\` or \`ts\` as the language tag.

Example:
\`\`\`tsx
// FILE: src/lib/ui-bridge/UIBridgeHooks.tsx
import { useSegments } from 'expo-router';
import {
  expoRouterPattern,
  expoRouterRouteStack,
  isExpoRouterUnmatched,
  useRouteAwareness,
} from '@qontinui/ui-bridge-native';

export function UIBridgeHooks() {
  // ... hook calls here
  return null;
}
\`\`\`

\`\`\`ts
// FILE: src/lib/ui-bridge/intents.ts
// Needs @qontinui/ui-bridge in the app's dependencies: an optional peer of ui-bridge-native.
import type { Intent } from '@qontinui/ui-bridge';

export const APP_INTENTS: Intent[] = [
  // ... intent definitions
];
\`\`\`

**If you do not output code blocks with \`// FILE:\` markers, the generation will fail.**
You may use file-reading tools to explore the codebase, but all generated code must be
output as text in the format above.

### File Structure Rules

1. **\`src/lib/ui-bridge/UIBridgeHooks.tsx\`** — Single component containing the global hooks:
   - \`useRouteAwareness()\` with the Expo Router helpers (the route template is the screen's identity; there is no page context on React Native)
   - Renders \`null\` — placed inside the UIBridgeNativeProvider tree

2. **\`src/lib/ui-bridge/intents.ts\`** — Intent definitions (only if intents category selected)

3. Per-component files only where \`useUIElement()\` or \`useUIComponent()\` must be colocated with a specific component.

**Import only what \`@qontinui/ui-bridge-native\` exports.** It does NOT export
\`useUIState\`, \`useUITransition\`, \`useUIStateGroup\`, \`useKeyboardShortcuts\`,
\`useUndoRedo\` or \`usePageContext\` — those are web hooks from the DOM barrel
\`@qontinui/ui-bridge/react\`, which must not be imported in a React Native app.
Before importing any name from \`@qontinui/ui-bridge-native\`, check that the
installed package's index exports it; if it does not, skip that hook and say so.
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

/** The quality rules of a React Native prompt: native APIs only. */
const RN_QUALITY_RULES = `
## Quality Rules

1. **Only generate code for things found in actual source code.** Never guess or fabricate.
2. **Use real component names, route templates, element IDs, and state variable names.**
3. Every \`State\` must wrap the subtree that really is that state, with a stable \`id\`.
4. Every \`TransitionTo\` must name real states in \`fromStates\` / \`activateStates\` / \`exitStates\`.
5. Generate no keyboard-shortcut, annotation or page-context code: the native SDK has none.
6. Do NOT declare undo state through the \`UndoTracker\` unless the app has an actual undo/redo system.
7. Import hooks and components from \`'@qontinui/ui-bridge-native'\` (not from internal paths, and never from \`'@qontinui/ui-bridge/react'\`).
8. Import types from \`'@qontinui/ui-bridge-native'\`, or type-only from \`'@qontinui/ui-bridge'\` — an optional peer the app must add to its own dependencies before importing from it.
9. The UIBridgeHooks component must render \`null\` — it's a hook container, not a visual component.
10. Spread \`bridgeProps\` together with \`ref\` and \`onLayout\` on every \`useUIElement\` host; never measure with web APIs.
`.trim();

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

/**
 * The exploration steps of a React Native prompt, built from the SELECTED
 * categories: a step (or a sample progress note) for a category the prompt
 * does not generate would send the session exploring for nothing. Page
 * context, keyboard shortcuts and annotations generate nothing on React
 * Native, so they contribute no step.
 */
const RN_EXPLORATION_STEPS: Partial<Record<HookCategory, string>> = {
  "route-awareness":
    "**Find the routes** — the Expo Router `app/` directory (or the React Navigation navigators)",
  "state-machine":
    "**Identify UI states** — modals, bottom sheets, drawers and the state that shows them",
  "undo-redo":
    "**Check for undo/redo** — look in state management (stores, reducers, context) for history/undo patterns",
  components:
    "**Identify major components** — screens and controls with significant actions or state",
  intents: "**Look for multi-step flows** — user journeys that span multiple screens",
};

/** One sample progress note per selected category that has a step. */
const RN_EXPLORATION_NOTES: Partial<Record<HookCategory, string>> = {
  "route-awareness": '"Found 12 screens under app/, including /(tabs)/prompts and /runner/[id]..."',
  "state-machine":
    '"Detected a filter bottom sheet (sheetOpen state in PromptsScreen) and 3 modals..."',
  "undo-redo": '"No undo/redo system found — skipping the undo category."',
  components: '"PromptsScreen exposes refresh and select-item; registering it as one component..."',
  intents: '"Found a 3-screen sign-up journey — one intent..."',
};

function rnExplorationInstructions(categories: HookCategory[]): string {
  const steps = [
    "**Read the layouts and screens** — understand the component tree, what wraps what",
    ...ALL_HOOK_CATEGORIES.filter((c) => categories.includes(c)).flatMap((c) =>
      RN_EXPLORATION_STEPS[c] ? [RN_EXPLORATION_STEPS[c]] : [],
    ),
  ];
  const notes = ALL_HOOK_CATEGORIES.filter((c) => categories.includes(c)).flatMap((c) =>
    RN_EXPLORATION_NOTES[c] ? [RN_EXPLORATION_NOTES[c]] : [],
  );
  return [
    "## CRITICAL: Read the Source Code First",
    "",
    "You MUST explore the actual source code before generating any hooks. Do not guess at app structure.",
    "",
    "**IMPORTANT: Use file-reading tools to explore the code, but do NOT use file-writing",
    "tools (Write, Edit, etc.) to create or modify any files. Your final output must be",
    "code blocks in your text response with `// FILE:` markers — see the Output Format section.**",
    "",
    "### Code exploration steps:",
    ...steps.map((step, i) => `${i + 1}. ${step}`),
    "",
    "### Progress communication",
    "Output brief progress notes as you explore. After each major discovery, write 1-2 sentences:",
    ...(notes.length > 0
      ? notes.map((n) => `- ${n}`)
      : ['- "Read the root layout and the screens it wraps..."']),
  ].join("\n");
}

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
  const isRN = isReactNativeFramework(analysis.framework);

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
  lines.push(isRN ? rnExplorationInstructions(categories) : EXPLORATION_INSTRUCTIONS);
  lines.push("");

  // Framework-specific guidance.
  // RN / Expo Router gets a dedicated branch with native imports and
  // measurement guidance; web frameworks fall through to the existing
  // Next.js / React Router blocks.
  const fw = analysis.framework.toLowerCase();
  if (isRN) {
    lines.push(RN_EXPO_GUIDANCE);
  } else if (fw === "nextjs" || fw === "next_js") {
    lines.push(NEXTJS_GUIDANCE);
  } else if (fw === "react") {
    lines.push(REACT_ROUTER_GUIDANCE);
  }
  lines.push("");

  // Hook API reference
  lines.push(hookApiReference(isRN));
  lines.push("");

  // Category-specific instructions (only for selected categories).
  // RN / Expo projects use an RN-flavored variant of every category that
  // replaces (not appends to) the web/DOM text — see RN_CATEGORY_INSTRUCTIONS.
  const categoryInstructions = isRN ? RN_CATEGORY_INSTRUCTIONS : CATEGORY_INSTRUCTIONS;
  lines.push("## Categories to Generate");
  lines.push("");
  for (const cat of categories) {
    lines.push(categoryInstructions[cat]);
    lines.push("");
  }

  // Output format
  lines.push(isRN ? RN_OUTPUT_FORMAT : OUTPUT_FORMAT);
  lines.push("");

  // Quality rules
  lines.push(isRN ? RN_QUALITY_RULES : QUALITY_RULES);

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
${
  isReactNativeFramework(analysis.framework)
    ? `6. **Remove every page-context hook from the existing code** — its import and every call.
   \`@qontinui/ui-bridge-native\` does NOT export \`usePageContext\`, and a native snapshot
   carries route info only, so it never belongs in a React Native app. Remove every
   import from the DOM barrel \`@qontinui/ui-bridge/react\` too.
`
    : ""
}
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

// Architecture spec format — describes tech stack, features, patterns, and dependencies.
// This is NOT the page spec format (groups + assertions). It's a project-level
// architecture description used by the Architecture page graph visualization.
const ARCHITECTURE_SPEC_FORMAT = `
# Architecture Spec Generation

Generate a project architecture spec in JSON format. This spec describes the tech stack,
features, patterns, and dependencies of the project — used for architecture visualization
and AI project understanding.

**IMPORTANT: Do NOT use file-writing tools. Output the spec as a single JSON code block
in your text response. The output will be parsed by a script.**

## Read the Source Code First

Explore the codebase to understand:
1. **Tech stack** — frameworks, libraries, languages, build tools, databases
2. **Features/modules** — major functional areas of the application
3. **Directory structure** — key directories and their purposes
4. **Architectural patterns** — state management, routing, API patterns, etc.
5. **Dependencies between features** — which features depend on, extend, or use others
6. **Agentic patterns** — AI/LLM integration, feedback loops, orchestrators, autonomous agents

Output brief progress notes as you explore.

## JSON Schema

Output a single \`\`\`json code block with this structure:

\`\`\`json
{
  "projectName": "My Project",
  "description": "Brief description of the project",
  "techStack": [
    {
      "name": "React",
      "category": "frontend | backend | database | build | testing | styling | state | routing | other",
      "version": "18.2.0",
      "purpose": "UI component framework"
    }
  ],
  "features": [
    {
      "id": "feature-id",
      "name": "Feature Name",
      "description": "What this feature does",
      "status": "active | planned | deprecated",
      "priority": "core | important | nice-to-have",
      "entryPoints": ["src/pages/FeaturePage.tsx"],
      "techUsed": ["React", "Redux"]
    }
  ],
  "patterns": [
    {
      "id": "pattern-id",
      "name": "Pattern Name",
      "description": "Description of the architectural pattern",
      "category": "state-management | routing | api | auth | error-handling | other",
      "usedBy": ["feature-id-1", "feature-id-2"]
    }
  ],
  "dependencies": [
    {
      "featureId": "feature-a",
      "dependsOn": "feature-b",
      "type": "requires | extends | uses",
      "description": "Why this dependency exists"
    }
  ],
  "directories": [
    {
      "path": "src/components",
      "purpose": "Shared React components",
      "required": true
    }
  ],
  "constraints": [
    {
      "id": "constraint-id",
      "description": "Important constraint or rule",
      "category": "performance | security | compatibility | convention"
    }
  ],
  "agenticStructure": {
    "agents": [
      {
        "id": "agent-id",
        "name": "Agent Name",
        "role": "orchestrator | executor | analyzer | session | monitor",
        "description": "What this agent does and what authority it has",
        "parentAgent": "parent-agent-id (optional, for nesting)",
        "capabilities": ["capability-1", "capability-2"],
        "delegatesTo": ["other-agent-id"],
        "triggers": ["what-activates-this-agent"],
        "decisionAuthority": ["what-decisions-this-agent-makes-autonomously"]
      }
    ],
    "feedbackLoops": [
      {
        "id": "loop-id",
        "name": "Loop Name",
        "description": "What cycles through this loop and why",
        "agents": ["agent-id-1", "agent-id-2"],
        "exitCondition": "When does the loop stop",
        "type": "inner | outer | cross"
      }
    ],
    "delegationChains": [
      {
        "from": "agent-id",
        "to": "agent-id",
        "via": "HTTP API | IPC | subprocess | direct call",
        "description": "How and why this delegation happens"
      }
    ]
  }
}
\`\`\`

## Agentic Structure Discovery

**If the project has AI/LLM integration or autonomous agents, populate the \`agenticStructure\` field.**
Skip this field entirely if the project has no agentic patterns.

Look for these patterns:
- **Retry/feedback loops** — \`while !success\`, iteration with max_iterations, verify-then-fix cycles
- **AI/LLM integration** — subprocess spawning of AI tools (Claude, GPT), API calls to LLM providers
- **Orchestrator patterns** — components that manage other components' lifecycles or coordinate multi-step processes
- **Verification authorities** — components that determine pass/fail and control whether a loop continues
- **Delegation chains** — HTTP calls between services, IPC between processes, subprocess spawning
- **Event-driven decisions** — event handlers that trigger different autonomous behavior paths
- **Error monitors** — systems that detect errors and autonomously trigger fixes

For each agent, clearly identify:
- What **role** it plays (orchestrator controls others, executor does work, analyzer examines results, session manages AI conversations, monitor watches for events)
- What **decisions** it makes autonomously (vs. what it delegates)
- What **triggers** activate it
- What it **delegates to** other agents

## Quality Rules

1. **Only include things found in actual source code.** Do not guess or fabricate.
2. **Use real names** — actual package names, real directory paths, real component names.
3. **Features should be functional areas**, not individual files. Group related code into features.
4. **Tech stack should include all significant dependencies** from package.json, Cargo.toml, pyproject.toml, etc.
5. **Dependencies should reflect real relationships** — if feature A imports from feature B, that's a dependency.
6. **Patterns should describe actual patterns in use**, not aspirational ones.
7. **Every feature needs a unique \`id\`** — use kebab-case (e.g., "workflow-builder", "terminal-page").
`.trim();

/**
 * Build an AI prompt for generating an architecture spec for the project.
 * Generates the architecture format (techStack/features/patterns/dependencies),
 * NOT the page spec format (groups/assertions).
 */
export function buildArchitectureSpecPrompt(analysis: ProjectAnalysisForHooks): string {
  return `${ARCHITECTURE_SPEC_FORMAT}

---

Create a comprehensive architecture spec for this project. Read the source code first,
then generate a complete JSON spec.

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
  return `${ARCHITECTURE_SPEC_FORMAT}

---

## Updating an Existing Architecture Spec

You are UPDATING an existing spec. The current spec is below. Read the source code and:
1. **Keep** features/tech/patterns that still exist in the code
2. **Add** new features/tech/patterns you discover
3. **Remove** items no longer in the codebase
4. **Enhance** descriptions with more detail where possible
5. **Preserve feature IDs** for items that haven't changed

### Existing Spec:

\`\`\`json
${existingSpec}
\`\`\`

---

Update this architecture spec by reading the current source code.

Framework detected: **${analysis.framework}**
Project path: \`${analysis.project_path}\``;
}
