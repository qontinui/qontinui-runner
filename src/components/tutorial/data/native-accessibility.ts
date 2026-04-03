/**
 * Native Accessibility Explorer Tutorial
 *
 * Informational tutorial explaining the Rust-native cross-platform
 * desktop accessibility layer — what it is, how it works, what the
 * UI shows, and how it connects to workflow automation.
 */

import type { Tutorial } from "../../../types/tutorial";

export const nativeAccessibilityTutorial: Tutorial = {
  id: "native-accessibility",
  title: "Native Desktop Accessibility",
  description:
    "Understand how the runner sees and interacts with any desktop application through native accessibility APIs — Windows UIA, Linux AT-SPI, and macOS AX.",
  duration: "8 minutes",
  difficulty: "intermediate",
  mode: "contextual",
  focusPage: "help",
  category: "Architecture",
  tags: ["accessibility", "native", "UIA", "AT-SPI", "automation", "featured", "architecture"],
  prerequisites: ["getting-started"],
  learningObjectives: [
    "Understand what the native accessibility layer is and why it exists",
    "Know the three platform adapters (UIA, AT-SPI, AX) and what they provide",
    "Understand the unified element model that normalizes across platforms",
    "See how the Accessibility Explorer UI lets you inspect live trees",
    "Learn how the native_accessibility step type enables workflow automation",
    "Understand the AI Context output that agents receive for desktop automation",
  ],
  steps: [
    {
      id: "intro",
      title: "What is the Native Accessibility Layer?",
      content: `Every desktop application exposes an **accessibility tree** — a semantic hierarchy describing its UI elements (buttons, text fields, menus, etc.) for screen readers and automation tools.

The runner has a **Rust-native layer** that connects directly to these platform APIs:

| Platform | API | What it provides |
|----------|-----|-----------------|
| **Windows** | UI Automation (UIA) | COM interface to all Win32/WPF/UWP apps |
| **Linux** | AT-SPI2 | D-Bus protocol for GTK/Qt/LibreOffice |
| **macOS** | AXAccessibility | CoreFoundation API for all macOS apps |

This replaces the older Python-based approach, which was slower (IPC overhead) and required optional dependencies.`,
      estimatedDuration: 1,
      tips: [
        "The accessibility tree is NOT the DOM — it's a semantic view optimized for automation",
        "This layer has nothing to do with CDP or browser automation — it's for native desktop apps",
      ],
    },
    {
      id: "why-not-screenshots",
      title: "Why Accessibility APIs Instead of Screenshots?",
      content: `You might wonder why the runner doesn't just use screenshots and vision models to interact with desktop apps. The answer is **speed and reliability**.

**Screenshot + Vision approach:**
- Capture screenshot (~50ms)
- Send to VisionLLM for element detection (~2000ms)
- Get approximate coordinates
- Click at coordinates (may miss, no semantic understanding)

**Accessibility API approach:**
- Capture tree (~200ms for a full window)
- Get exact element identity, role, name, bounds
- Click via native pattern (InvokePattern, not coordinates)
- Deterministic — no AI hallucination risk

The accessibility approach is **10x faster** and **semantically precise**. The runner uses InvokePattern to "click" a button through the OS, not by moving the mouse cursor.`,
      estimatedDuration: 1,
      details: `The visual approach (template matching, OmniParser, UI-TARS) is still available as a fallback for apps with poor accessibility support. The cascade detector tries accessibility first, then falls back to vision:

\`\`\`
Tier 1: Accessibility API (~200ms)
Tier 2: Template Matching (~10ms)
Tier 3: Feature Matching (~100ms)
Tier 4: QATM Deep Features (~200ms)
Tier 5: VisionLLM (~2000ms)
\`\`\``,
    },
    {
      id: "unified-model",
      title: "The Unified Element Model",
      content: `Each platform API has its own way of describing elements. UIA uses "control types" (50000=Button), AT-SPI uses role strings ("push button"), macOS uses role names ("AXButton"). The runner normalizes all of these into a single **UnifiedNode** model:

\`\`\`
UnifiedNode {
  ref: "@e3"           // Stable reference ID
  role: "button"       // Normalized across platforms
  name: "Submit"       // Accessible name/label
  value: null          // Current value (for inputs)
  bounds: {x, y, w, h} // Screen coordinates
  state: {focused, disabled, hidden, ...}
  is_interactive: true
  supported_patterns: [Invoke, Toggle]
  source: "uia"        // Which adapter produced this
}
\`\`\`

The **UnifiedRole** enum has ~100 variants covering ARIA roles, UIA extensions, and AX-specific roles. This means the same query code works on all platforms.`,
      estimatedDuration: 1,
      tips: [
        "The ref ID (@e1, @e2) persists across captures via fingerprinting, so you can reference elements across sessions",
        "The 'source' field tells you which platform adapter produced each node",
      ],
    },
    {
      id: "interaction-patterns",
      title: "Native Interaction Patterns",
      content: `The old approach clicked at pixel coordinates. The new layer uses **native accessibility patterns** — the same mechanisms screen readers use:

| Pattern | What it does | Example |
|---------|-------------|---------|
| **Invoke** | Activates an element | Click a button without moving the mouse |
| **Value** | Sets text content | Type into a field via the OS, not keystrokes |
| **Toggle** | Flips a toggle/checkbox | Check/uncheck without clicking |
| **ExpandCollapse** | Opens/closes menus, trees | Expand a dropdown programmatically |
| **RangeValue** | Sets slider position | Move a slider to a specific value |
| **Scroll** | Scrolls a container | Scroll without mouse wheel events |

The **InteractionEngine** picks the best pattern for each action:

1. Try native pattern (Invoke for clicks, Value for typing)
2. Fall back to coordinate-based clicking if no pattern available

This is more reliable because it uses the OS's own mechanism — it can't "miss" a button.`,
      estimatedDuration: 1,
    },
    {
      id: "explorer-ui",
      title: "The Accessibility Explorer UI",
      content: `The **Accessibility Explorer** panel (in the sidebar under Dev) lets you inspect live accessibility trees.

**How it works:**
1. Enter a window title (e.g., "Notepad") in the target field
2. Click **Connect** — the runner connects via the platform adapter
3. The tree is automatically captured and displayed

**What you see:**
- **Tree View** (left 60%) — hierarchical tree with role badges, names, and ref IDs
- **Details Panel** (right 40%) — full properties of the selected node
- **Query tab** (bottom) — search by role, label, or interactivity
- **AI Context tab** (bottom) — the exact text AI agents receive

Each tree node shows a color-coded role badge:
- 🔵 Blue = buttons, links, menu items, tabs
- 🟢 Green = text inputs, checkboxes, sliders
- ⚫ Gray = static text, labels, headings
- 🟣 Purple = other roles`,
      estimatedDuration: 1,
      tips: [
        "Try connecting to 'Desktop' to see all open windows at once",
        "You can use 'pid:1234' to connect to a specific process by ID",
        "Click Refresh to re-capture the tree after the target app changes",
      ],
    },
    {
      id: "details-and-actions",
      title: "Element Details and Actions",
      content: `Click any node in the tree to see its full details:

**Properties shown:**
- Role, Name, Value, Description
- Ref ID (copyable — use in workflows)
- Bounds (x, y, width, height in screen pixels)
- State flags as badges (focused, disabled, hidden, readonly, etc.)
- Supported patterns (Invoke, Value, Toggle, etc.)
- Source adapter (UIA, ATSPI, AX)

**Actions you can perform:**
- **Click** — executes via the best native pattern (InvokePattern on Windows)
- **Focus** — moves keyboard focus to the element
- **Type...** — reveals a text input to type into the element via ValuePattern

Action results appear inline: green for success, red for errors. This lets you verify that accessibility automation actually works before putting it in a workflow.`,
      estimatedDuration: 1,
    },
    {
      id: "query-and-ai-context",
      title: "Query Bar and AI Context",
      content: `The bottom section has two tabs:

**Query tab:**
- Select a role from 25 common types (button, textbox, slider, etc.)
- Enter a label substring (e.g., "Submit")
- Toggle "Interactive only" to filter non-interactive elements
- Click Search to find matching nodes
- Click a result to select it in the tree

**AI Context tab:**
- Click "Generate AI Context" to see the formatted text
- This is the **exact output** that AI workflow agents receive when a \`native_accessibility\` step runs with \`ai_context\` action
- Format: markdown with refs, roles, names, and state flags
- Copy button to paste into prompts or documentation

Example AI Context output:
\`\`\`
## Accessibility Tree
Title: Calculator

### Interactive Elements
- @e3: button "Two"
- @e4: button "Plus"
- @e5: button "Three"
- @e6: button "Equals"
\`\`\``,
      estimatedDuration: 1,
      tips: [
        "The AI Context uses interactive_only=true by default, showing only clickable elements",
        "The format matches the Python IAccessibilityCapture.to_ai_context() output exactly",
      ],
    },
    {
      id: "workflow-integration",
      title: "Using in Workflows",
      content: `The \`native_accessibility\` step type brings this into automated workflows:

\`\`\`yaml
steps:
  - name: "Connect to Calculator"
    type: native_accessibility
    a11y_action: capture
    a11y_target: "Calculator"

  - name: "Click number 2"
    type: native_accessibility
    a11y_action: click
    a11y_ref_id: "@e3"

  - name: "Get AI context"
    type: native_accessibility
    a11y_action: ai_context
    a11y_interactive_only: true
\`\`\`

**Available actions:**
- \`capture\` — connect + capture tree (auto-connects if not connected)
- \`click\` / \`type\` / \`focus\` — interact with an element by ref
- \`query\` — find elements by role/label
- \`ai_context\` — generate the AI-readable tree summary

The step handler checks the \`use_rust_accessibility\` setting — if disabled, the step fails with a descriptive error.`,
      estimatedDuration: 1,
      details: `The feature flag \`use_rust_accessibility\` (in Accessibility Settings, default: true) controls whether the Rust-native layer is active. When disabled, the Python HAL backends are used instead.

The Python side auto-detects the Rust backend: if the runner is available and \`use_rust_accessibility\` is true, it uses \`RustBackendCapture\` (HTTP bridge to the Rust layer). Otherwise it falls back to the platform-native Python backends (uiautomation, pyatspi2).`,
    },
    {
      id: "architecture-summary",
      title: "Architecture at a Glance",
      content: `Here's how everything fits together:

\`\`\`
Tauri Commands / MCP API / Step Executor
           |
   AccessibilityManager
     |           |
 QueryEngine  InteractionEngine
     |           |
 TreeCache + EventBus
     |
 FusionEngine (reserved)
     |
 PlatformAdapter
     |
 +---------+---------+
 |         |         |
UIA     AT-SPI      AX
(Win)   (Linux)   (macOS)
\`\`\`

**Key components:**
- **AccessibilityManager** — top-level facade, manages adapters and cache
- **TreeCache** — holds the latest snapshot, supports incremental patching via events
- **QueryBuilder** — composable filters: \`by_role(Button).by_label("Submit").interactive()\`
- **InteractionEngine** — picks the best native pattern for each action
- **RefManager** — assigns stable @e1 refs with cross-session persistence
- **FusionEngine** — reserved for future merging of UI Bridge data with native trees

The entire layer compiles to ~9,000 lines of Rust with 46 unit tests and 4 integration tests (verified against live Notepad on Windows).`,
      estimatedDuration: 1,
      tips: [
        "The FusionEngine is reserved for merging UI Bridge data with native trees in the future",
        "Integration tests captured 39 nodes from Notepad in ~200ms and 90 desktop nodes in ~350ms",
      ],
      resources: [
        {
          title: "AccessKit (cross-platform a11y infrastructure)",
          url: "https://github.com/AccessKit/accesskit",
        },
        {
          title: "odilia-app/atspi (Rust AT-SPI2 consumer)",
          url: "https://github.com/odilia-app/atspi",
        },
      ],
    },
  ],
};
