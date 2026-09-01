# Create Interactive Tutorial

Create an interactive tutorial for a Qontinui feature or workflow.

**Input**: `$ARGUMENTS` — a description of what the tutorial should cover. Include:
- The feature or page to tutorial-ize
- Whether this is **informational** (explains concepts/architecture to the developer), **operational** (walks a user through GUI actions), or **both**
- Target app: `runner`, `web`, or both

If no type is specified, determine from context: conceptual/architectural topics default to informational, action-oriented topics default to operational.

---

## Tutorial System Background

Qontinui has a production-grade, custom interactive tutorial framework (no external libraries). Tutorials appear as contextual in-page experiences with spotlight overlays, positioned tooltips, and event-driven step progression.

### Architecture Overview

**Two apps, two implementations with shared patterns:**

| Aspect | Runner (Tauri desktop) | Web (Next.js) |
|--------|----------------------|---------------|
| Types | `qontinui-runner/src/types/tutorial.ts` | `qontinui-web/frontend/src/types/tutorial.ts` |
| Context | `qontinui-runner/src/contexts/TutorialContext.tsx` | `qontinui-web/frontend/src/contexts/tutorial/TutorialContext.tsx` |
| Store | Context-based with instanceStorage | Zustand store (`stores/tutorial-store.ts`) |
| Data | `qontinui-runner/src/components/tutorial/data/` | `qontinui-web/frontend/src/components/tutorial/data/` |
| Modes | `contextual` only | `overlay`, `contextual`, `hybrid` |
| Events | DOM + Tauri events + app-action | DOM + app-action + route-change |
| Components | `qontinui-runner/src/components/tutorial/` | `qontinui-web/frontend/src/components/tutorial/` |

### Key Types (Runner — simpler, canonical reference)

```typescript
interface Tutorial {
  id: string;                          // Unique kebab-case identifier
  title: string;                       // Display title
  description: string;                 // 1-2 sentence summary
  duration: string;                    // e.g., "10 minutes"
  difficulty: "beginner" | "intermediate" | "advanced";
  mode: "contextual";                  // Runner only supports contextual
  focusPage?: TutorialFocusPage;       // Page to navigate to when starting
  category: string;                    // e.g., "Getting Started", "Workflow Building", "AI Features"
  tags: string[];                      // For search/filtering, include "featured" for prominence
  prerequisites?: string[];            // Tutorial IDs that must be completed first
  learningObjectives?: string[];       // What the user will learn
  steps: TutorialStep[];
}

interface TutorialStep {
  id: string;                          // Unique within tutorial
  title: string;                       // Step heading
  content: string;                     // Markdown-formatted explanation
  targetElement?: TargetElement;       // Element to highlight (omit for centered modal)
  action?: string;                     // Instruction text (e.g., "Click the Save button")
  wait?: StepWaitCondition;            // Event-driven auto-advancement
  validation?: StepValidation;         // Interactive validation
  interactive?: boolean;               // Allow element interaction during step
  estimatedDuration?: number;          // Minutes
  tips?: string[];                     // Extra tips shown in tooltip
  difficulty?: DifficultyLevel;
  details?: string;                    // Expandable extra content
  shortcuts?: { key: string; description: string }[];
  resources?: { label: string; url: string }[];
  screenshot?: string;                 // Path to screenshot image
  prepare?: () => void | Promise<void>;   // Run before step displays
  complete?: () => void | Promise<void>;  // Run after step completes
}

interface TargetElement {
  selector: string;                    // CSS selector or data-tutorial-id value
  highlightType: "spotlight" | "border" | "pulse" | "arrow";
  position: "top" | "bottom" | "left" | "right" | "center";
  allowInteraction?: boolean;          // Let user click the highlighted element
  scrollIntoView?: boolean;
  delay?: number;                      // ms before highlighting
  offset?: { x: number; y: number };
}

interface StepWaitCondition {
  type: "dom-event" | "tauri-event" | "dom-appear" | "app-action";
  event?: string;        // DOM event name (click, input, change)
  selector?: string;     // Element to listen on
  tauriEvent?: string;   // Tauri backend event name
  actionName?: string;   // Custom app action name
  filter?: (eventData: unknown, tourState: TourState) => boolean;
  timeout?: number;      // ms
  onTimeout?: "show-hint" | "allow-skip";
  hint?: string;         // Shown on timeout
  advanceDelay?: number; // ms delay before advancing
}
```

### Web-Specific Additions

The web version adds:
- `mode: "overlay" | "contextual" | "hybrid"` — overlay shows a full dialog with sidebar
- `WaitEventType` includes `"route-change"` for Next.js navigation
- `TryItConfig` — interactive exercises with preloaded data
- `Annotation[]` — screenshot markup (highlight, arrow, pulse, label)
- Safe evaluation system (`lib/safe-eval.ts`) for serialized conditions
- `estimatedTime: number` field on Tutorial

### Focus Pages

**Runner** `TutorialFocusPage`: `gui-automation`, `run`, `active`, `unified-workflow-builder`, `workflow-builder`, `macro-builder`, `playwright-test-builder`, `check-builder`, `library`, `ai`, `settings`, `help`

**Web** `TutorialFocusPage`: `projects`, `build`, `settings`, `analytics`, etc.

### How Elements Are Targeted

1. **`data-tutorial-id` attributes** — Components register themselves: `<div data-tutorial-id="sidebar-main">`
2. **CSS selectors** — For elements without tutorial IDs: `'[data-tutorial-id="sidebar-main"]'` or standard CSS
3. **Target registration** — Components call `registerTarget(id, element)` via TutorialContext

### Tutorial Data Files

Each tutorial is a separate `.ts` file exporting a `Tutorial` object, registered in `data/index.ts`.

**Runner registry**: `qontinui-runner/src/components/tutorial/data/index.ts`
**Web registry**: `qontinui-web/frontend/src/components/tutorial/data/index.ts`

### Existing Tutorials (Runner)

| ID | Title | Focus Page | Category |
|----|-------|------------|----------|
| `getting-started` | Welcome to Qontinui Runner | gui-automation | Getting Started |
| `prompt-workflow` | Build an AI Prompt Workflow | unified-workflow-builder | Workflow Building |
| `workflow-execution` | Running Your First Workflow | gui-automation | Execution |
| `ai-analysis` | AI-Powered Automation | ai | AI Features |
| `check-builder` | Master Code Quality Checks | check-builder | Build Tools |

### Two Tutorial Types

**Informational** tutorials explain concepts and architecture to the developer:
- Focus on *why* things exist and how they fit together
- Steps explain the mental model, design decisions, data flow
- May reference code architecture, algorithms, or integration patterns
- Less interactivity, more detailed content with `details` expandable sections
- Good use of `tips`, `resources` links, and architecture diagrams
- Target audience: the developer (Joshua) wanting to understand the system

**Operational** tutorials walk users through performing actions:
- Focus on *how* to achieve specific goals in the GUI
- Steps have `targetElement` highlighting, `action` instructions
- Use `wait` conditions for event-driven progression (user clicks, elements appear)
- Interactive: `allowInteraction: true` on targets so users can follow along
- Progressive disclosure: start simple, build complexity
- Target audience: users learning to use the product

**Combined** tutorials blend both: explain the concept, then walk through using it.

---

## Instructions

### Phase 1: Research the Feature

**You MUST understand the feature before writing a tutorial.** Do ALL of the following:

1. **Find the page/component** — Search for the page component file in the target app
2. **Read page specs** — Check for `.spec.uibridge.json` files in:
   - Runner: `qontinui-runner/src/specs/`
   - Web: co-located with page components
   Page specs document the page structure, element groups, assertions, and expected behavior. They are the closest thing to a functional specification. Note: specs vary in quality — some are comprehensive, others are stubs.
3. **Read the component source** — Understand what the page renders, its sections, panels, forms, buttons, tabs
4. **Read child components** — Follow imports to understand the full UI tree
5. **Read hooks and state** — Understand data flow, API calls, state management
6. **Check for `data-tutorial-id` attributes** — These are existing tutorial targets in the JSX
7. **Read related tutorials** — Check if any existing tutorials cover adjacent features
8. **Understand the domain** — For informational tutorials, read the underlying library/service code to understand the architecture and algorithms

### Phase 2: Plan the Tutorial

Design the tutorial structure:

**For informational tutorials:**
- Start with the "big picture" — what is this feature and why does it exist?
- Break down the architecture into digestible concepts
- Explain how this feature connects to the broader Qontinui system
- Include technical details in `details` expandable sections
- End with a mental model summary

**For operational tutorials:**
- Start with context — what will the user accomplish?
- Map out the user journey: what actions, in what order?
- Identify which UI elements need `data-tutorial-id` attributes (and whether they already have them)
- Plan event-driven steps where the user should actually perform actions
- End with a "what's next" step pointing to related tutorials

**For both types:**
- 8-15 steps is typical (fewer for focused tutorials, more for comprehensive ones)
- Each step should take 30-90 seconds
- Group related concepts into single steps rather than creating too many tiny steps

### Phase 3: Check for Missing Tutorial Targets

If the tutorial needs to highlight elements that don't have `data-tutorial-id` attributes:
1. List the elements that need targeting
2. Add `data-tutorial-id="descriptive-kebab-case"` attributes to the relevant components
3. If the component uses `useTutorialTarget` hook (web), use that pattern instead

### Phase 4: Write the Tutorial Data File

Create a new file in the appropriate data directory:

**Runner**: `qontinui-runner/src/components/tutorial/data/{tutorial-id}.ts`
**Web**: `qontinui-web/frontend/src/components/tutorial/data/{tutorial-id}.ts`

Follow the patterns from existing tutorials (check-builder.ts is a good runner example, onboarding-tour.ts for web).

#### Step Content Guidelines

- Use markdown in `content` strings — **bold**, lists, code blocks all render
- Keep each step's content to 3-6 lines — enough to explain, not overwhelm
- Use `action` for clear user instructions: "Click the Save button"
- Use `tips` for non-essential helpful info
- Use `details` for expandable deep-dive content (great for informational tutorials)
- For informational steps without a target element, the tooltip renders as a centered modal

#### Highlight Type Selection

| Type | When to use |
|------|-------------|
| `spotlight` | Primary focus element — dark overlay draws attention |
| `pulse` | Call-to-action — element pulses to invite clicking |
| `border` | Secondary highlight — subtle border without darkening page |
| `arrow` | Pointing to something specific within a larger area |

#### Wait Condition Patterns

```typescript
// Wait for user to click a specific button
wait: {
  type: "dom-event",
  event: "click",
  selector: '[data-tutorial-id="save-button"]',
  timeout: 30000,
  onTimeout: "show-hint",
  hint: "Click the Save button to continue",
}

// Wait for element to appear (after navigation or async load)
wait: {
  type: "dom-appear",
  selector: '[data-tutorial-id="workflow-panel"]',
  timeout: 10000,
  onTimeout: "allow-skip",
}

// Wait for Tauri backend event (runner only)
wait: {
  type: "tauri-event",
  tauriEvent: "workflow-saved",
  timeout: 15000,
  onTimeout: "show-hint",
  hint: "Save the workflow to continue",
}

// Wait for custom app action
wait: {
  type: "app-action",
  actionName: "tab-changed",
  filter: (data) => data?.tab === "verification",
  timeout: 20000,
  onTimeout: "allow-skip",
}
```

### Phase 5: Register the Tutorial

Add the tutorial to the data registry:

**Runner** (`qontinui-runner/src/components/tutorial/data/index.ts`):
```typescript
import { myNewTutorial } from "./my-new-tutorial";

export const tutorials: Tutorial[] = [
  // ... existing tutorials
  myNewTutorial,
];

// Add to re-exports
export { myNewTutorial } from "./my-new-tutorial";
```

**Web** (`qontinui-web/frontend/src/components/tutorial/data/index.ts`):
Same pattern — import and add to the tutorials array.

### Phase 6: Summary

Print a summary:
- Tutorial ID, title, type (informational/operational/both)
- Target app (runner/web)
- Number of steps, estimated duration
- Focus page
- Any `data-tutorial-id` attributes that were added to components
- Prerequisites (if any)
- How to test: "Open the app, go to Help tab, find the tutorial and start it"
