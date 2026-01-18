# Unified Workflow Builder Design

## Executive Summary

This document proposes consolidating AI Workflows, GUI Workflows, Tasks, and all step types into a **single unified Workflow system**. Every workflow is a sequence of optional steps organized into three phases, and features (orchestration, verification, etc.) are enabled automatically based on which step types are present.

---

## 0. Core Workflow Model

### Workflow Execution Order

Every workflow follows this three-phase execution model:

```
┌─────────────┐     ┌──────────────┐     ┌─────────────┐
│   SETUP     │ ──► │ VERIFICATION │ ──► │   AGENTIC   │
└─────────────┘     └──────────────┘     └─────────────┘
                           ▲                    │
                           │                    │
                           └────────────────────┘
                              (loop until pass)
```

| Phase | Purpose | Runs |
|-------|---------|------|
| **Setup** | Navigate to the testing point | Depends on step type (see below) |
| **Verification** | Check current state, identify issues | Before each agentic iteration |
| **Agentic** | AI performs work to fix issues | Until verification passes |

### Setup Step Execution Frequency

| Step Type | Default | Configurable |
|-----------|---------|--------------|
| **Qontinui Automation** (GUI Actions, State Navigation, Workflows) | Once at start | Yes - can run each iteration |
| **Playwright Scripts** | Every iteration | No - browser state not preserved |

**Qontinui-style workflow:**
```
SETUP (once) ──► VERIFICATION ◄──► AGENTIC (loop)
```

**Playwright-style workflow:**
```
Each iteration: SETUP (script) → VERIFICATION (test) → AGENTIC (prompt)
                      └─────────────────────────────────────────────────┘
                                    (browser state not preserved)
```

### Key Design Decisions

1. **Dropdown organized by phase** - Helps users visualize when each step runs
2. **GUI Actions duplicated in Setup and Verification** - Same action types serve different purposes
3. **Steps locked within phase groups** - No cross-phase reordering
4. **Collapsible phase sections** - Visual separation in step list
5. **Scripts = Playwright only** - Setup navigation to testing point
6. **Capture = Verification** - Screenshots check current state

---

## 1. Current State Analysis

### 1.1 Current Fragmentation

| Builder | Location | Step Types | Storage |
|---------|----------|------------|---------|
| AI Workflows | `/workflow-builder` | 8 types (workflow, state, playwright, prompt, action, screenshot, gui_workflow, test) | `ai-workflows` |
| GUI Workflows | `/gui-workflow-builder` | 6 types (click, double_click, right_click, type, hotkey, go_to_state) | `gui-workflows` |
| Tasks | Library only | Single prompt | `prompts` |
| Scripts | `/script-builder` | Playwright scripts | `playwright/scripts` |
| Tests | `/test-builder` | 4 types (playwright_cdp, qontinui_vision, python_script, repository_test) | `verifications` |

**Problems:**
- 5 different places to create automation items
- Overlapping capabilities (GUI steps in AI Builder vs GUI Workflow Builder)
- Tasks are just single-prompt workflows but stored separately
- No unified way to combine all step types

### 1.2 Current Navigation Structure

```
BUILD
├── Library (browse all items)
├── AI Workflows (multi-step with AI)
├── GUI Workflows (deterministic actions)
├── Scripts (Playwright code)
├── Tests (verification tests)
└── Capture (screen capture)
```

---

## 2. Unified Step Type System

### 2.1 Step Types by Phase

#### SETUP Phase Steps
| Step Type | Icon | Description | Example |
|-----------|------|-------------|---------|
| **Script** | 📜 | Playwright script to navigate to testing point | Login, navigate to dashboard |
| **State** | 📍 | Navigate to stored app state | Go to "Login Page" state |
| **Workflow** | 🔄 | Execute another workflow | Run "Login" workflow |
| **GUI Action** | 🖱️ | Mouse/keyboard for setup | Click menu, type credentials |

**Scripts vs Tests:**
- **Scripts** = Playwright only, for setup/navigation
- Created with AI or manually
- Has refinement loop until succeeds
- Brings automation to the testing point (does NOT test target functionality)

#### VERIFICATION Phase Steps
| Step Type | Icon | Description | Example |
|-----------|------|-------------|---------|
| **Test** | ✓ | Verify target functionality | Build check, type check, Playwright test |
| **Screenshot** | 📷 | Capture current state | Screenshot for AI analysis |
| **GUI Action** | 🖱️ | Mouse/keyboard for verification | Click to reveal state |

**Test Types:**
- **Playwright Test** - Browser assertions (fused with Script state at runtime)
- **Qontinui Vision Test** - Visual element detection
- **Python Test** - White-box unit tests
- **Repository Test** - Run tests that live in repo (pytest, jest, cargo test)
- **Custom Command** - Any shell command

**Tests:**
- Created with AI or manually
- Has analysis tools to identify page elements before writing prompts
- Tests the actual target functionality

#### AGENTIC Phase Steps
| Step Type | Icon | Description | Example |
|-----------|------|-------------|---------|
| **Prompt** | 💬 | AI task instructions | "Fix all TypeScript errors" |

### 2.2 Step Type Details

#### Script Step (Setup only, Playwright)
```typescript
interface ScriptStep {
  type: 'script';
  phase: 'setup';
  name: string;
  code?: string;              // Inline Playwright code
  script_id?: string;         // Reference to saved script
  target_url?: string;        // Starting URL
  refinement_enabled: boolean; // Loop until succeeds
}
```

#### Test Step (Verification only)
```typescript
interface TestStep {
  type: 'test';
  phase: 'verification';
  name: string;
  test_type: 'playwright' | 'qontinui_vision' | 'python' | 'repository' | 'custom_command';
  command?: string;           // For custom/repository commands
  code?: string;              // For inline Playwright/Python
  script_id?: string;         // Reference to saved test
  is_critical: boolean;       // Blocks workflow if fails

  // Playwright test configuration
  fused_script_id?: string;   // Which setup script to fuse with
  execution_mode?: 'independent' | 'chained';  // Fresh session vs continue after previous test
}
```

**Playwright Test Execution Modes:**

| Mode | Behavior | Use Case |
|------|----------|----------|
| **independent** | Script runs fresh, then test runs (new browser session) | Parallel-friendly, isolated tests |
| **chained** | Test runs after previous test in same session | Sequential tests sharing state |

**Example Configurations:**
```
Independent tests:
  Script A → Test A  (session 1)
  Script A → Test B  (session 2)

Chained tests:
  Script A → Test A → Test B → Test C  (same session)

Mixed:
  Script A → Test A → Test B  (session 1, chained)
  Script A → Test C           (session 2, independent)
```

#### Screenshot Step (Verification only)
```typescript
interface ScreenshotStep {
  type: 'screenshot';
  phase: 'verification';
  name: string;
  delay_ms?: number;
  monitor?: 'all' | 'primary' | 'left' | 'right';
}
```

#### GUI Action Step (Setup or Verification)
```typescript
interface GuiActionStep {
  type: 'gui_action';
  phase: 'setup' | 'verification';
  name: string;
  action: 'click' | 'double_click' | 'right_click' | 'type' | 'hotkey' | 'scroll';
  target_image_ids?: string[];  // For click actions
  text_input?: string;          // For type action
  hotkey?: string;              // For hotkey action
  scroll_direction?: 'up' | 'down';
  pause_after_ms?: number;
  monitor_index?: number;
}
```

#### State Step (Setup only)
```typescript
interface StateStep {
  type: 'state';
  phase: 'setup';
  name: string;
  state_id: string;           // Reference to stored state
  timeout_seconds?: number;
}
```

#### Workflow Step (Setup only)
```typescript
interface WorkflowStep {
  type: 'workflow';
  phase: 'setup';
  name: string;
  workflow_id: string;        // Reference to another workflow
}
```

#### Prompt Step (Agentic only)
```typescript
interface PromptStep {
  type: 'prompt';
  phase: 'agentic';
  name: string;
  content: string;           // The prompt text
  provider?: string;         // AI provider override
  model?: string;            // Model override
}
```

### 2.3 Unified Step Union Type

```typescript
type Phase = 'setup' | 'verification' | 'agentic';

type UnifiedStep =
  | ScriptStep       // Setup only
  | StateStep        // Setup only
  | WorkflowStep     // Setup only
  | GuiActionStep    // Setup or Verification
  | TestStep         // Verification only
  | ScreenshotStep   // Verification only
  | PromptStep;      // Agentic only

interface Workflow {
  id: string;
  name: string;
  description: string;

  // Steps organized by phase
  setup_steps: (ScriptStep | StateStep | WorkflowStep | GuiActionStep)[];
  verification_steps: (TestStep | ScreenshotStep | GuiActionStep)[];
  agentic_steps: PromptStep[];

  // Auto-computed properties (based on steps)
  has_setup: boolean;
  has_verification: boolean;
  has_agentic: boolean;

  // Settings (shown when relevant steps present)
  max_iterations?: number;        // When has_agentic

  // Metadata
  category: string;
  tags: string[];
  created_at: string;
  modified_at: string;
}
```

---

## 3. Automatic Feature Enablement

### 3.1 Feature Detection Logic

```typescript
function detectFeatures(steps: UnifiedStep[]): WorkflowFeatures {
  const hasPrompt = steps.some(s => s.type === 'prompt');
  const hasTests = steps.some(s => s.type === 'test');
  const hasGuiActions = steps.some(s => s.type === 'gui_action');
  const hasStates = steps.some(s => s.type === 'state');
  const hasWorkflows = steps.some(s => s.type === 'workflow');

  return {
    // Orchestration enabled when AI prompts present
    orchestration: hasPrompt,

    // Verification loop enabled when tests present
    verification: hasTests,

    // Config selection required when GUI-dependent steps present
    requiresConfig: hasGuiActions || hasStates || hasWorkflows,

    // Iteration settings shown when orchestration enabled
    showIterationSettings: hasPrompt,

    // Verification-first option shown when tests + prompt
    showVerificationFirst: hasPrompt && hasTests,

    // Single-pass execution when no prompts
    singlePass: !hasPrompt,
  };
}
```

### 3.2 UI Adaptation

Based on detected features, the builder UI adapts:

| Steps Present | UI Shows |
|---------------|----------|
| Only Prompt | Simple task mode - just prompt editor + basic settings |
| Prompt + Tests | Orchestrated mode - goal input, verification criteria, iteration settings |
| Only GUI Actions | Deterministic mode - linear sequence, no AI settings |
| GUI + Prompt | Full mode - config selector, AI settings, GUI preview |
| Only Tests | Verification-only mode - test runner configuration |

---

## 4. Navigation Redesign

### 4.1 Proposed Navigation

```
BUILD
├── Library (browse all workflows)
├── Workflow Builder (unified builder)
│   ├── Canvas (main step editing)
│   └── Components (tabbed panel)
│       ├── Prompts (saved prompts library)
│       ├── Scripts (Playwright/Python scripts)
│       ├── Tests (verification tests)
│       ├── GUI Actions (click targets, states)
│       └── Workflows (other workflows to reference)
└── Capture (screen/state capture)
```

### 4.2 Alternative: Sidebar-Based Builder

```
Workflow Builder Page
┌─────────────────────────────────────────────────────────────┐
│ [Workflow Name]                    [Save] [Run] [Settings]  │
├───────────────┬─────────────────────────────────────────────┤
│               │                                             │
│ STEPS         │  STEP EDITOR                                │
│               │                                             │
│ + Add Step    │  [Selected step configuration]              │
│               │                                             │
│ ┌───────────┐ │  ┌─────────────────────────────────────┐    │
│ │ 1. Prompt │ │  │ Step Name: [____________]            │    │
│ └───────────┘ │  │                                     │    │
│ ┌───────────┐ │  │ Prompt Content:                     │    │
│ │ 2. Test   │ │  │ [                                 ] │    │
│ └───────────┘ │  │ [                                 ] │    │
│ ┌───────────┐ │  │ [                                 ] │    │
│ │ 3. Script │ │  │                                     │    │
│ └───────────┘ │  │ Provider: [Claude ▼]                │    │
│               │  └─────────────────────────────────────┘    │
│ ─────────────── │                                           │
│               │  COMPONENTS (collapsible)                   │
│ SETTINGS      │  ┌──────┬──────┬──────┬──────┐              │
│ (auto-shown)  │  │Prompts│Scripts│Tests │GUI   │             │
│               │  └──────┴──────┴──────┴──────┘              │
│ Max Iterations│  [Saved items to drag into steps]           │
│ [10        ]  │                                             │
│               │                                             │
│ □ Verify First│                                             │
│               │                                             │
└───────────────┴─────────────────────────────────────────────┘
```

### 4.3 Phase-Based Add Step Menu

The dropdown is organized by workflow phase to help users understand when each step runs:

```
+ Add Step
│
├── 1. SETUP (runs once at start)
│   ├── Playwright Script (📜)
│   │   └── Navigate to testing point
│   ├── Navigate to State (📍)
│   │   └── Go to stored app state
│   ├── Run Workflow (🔄)
│   │   └── Execute another workflow
│   └── GUI Actions
│       ├── Click (🖱️)
│       ├── Double-Click (🖱️)
│       ├── Right-Click (🖱️)
│       ├── Type (⌨️)
│       ├── Hotkey (⌨️)
│       └── Scroll (↕️)
│
├── 2. VERIFICATION (checks state, loops with agentic)
│   ├── Tests
│   │   ├── Playwright Test (🎭)
│   │   ├── Qontinui Vision Test (👁️)
│   │   ├── Python Test (🐍)
│   │   ├── Repository Test (📦)
│   │   └── Custom Command (⚡)
│   ├── Capture
│   │   └── Screenshot (📷)
│   └── GUI Actions
│       ├── Click (🖱️)
│       ├── Double-Click (🖱️)
│       ├── Right-Click (🖱️)
│       ├── Type (⌨️)
│       ├── Hotkey (⌨️)
│       └── Scroll (↕️)
│
└── 3. AGENTIC (AI work, iterates until verification passes)
    └── Prompt (💬)
        └── AI task instructions
```

**Key Points:**
- GUI Actions appear in both Setup and Verification (duplicated)
- User selects the phase when adding a step
- Steps are locked within their phase (no cross-phase reordering)

---

## 5. Component Library Panel

### 5.1 Purpose

The Components panel provides access to saved/reusable items that can be added to workflows:

| Tab | Contents | Action |
|-----|----------|--------|
| **Prompts** | Saved prompt templates | Insert as Prompt step |
| **Scripts** | Playwright/Python scripts | Insert as Script step |
| **Tests** | Saved verification tests | Insert as Test step |
| **GUI** | Stored states, images, actions | Insert as State/GUI Action step |
| **Workflows** | Other saved workflows | Insert as Workflow step |

### 5.2 Interaction Patterns

1. **Drag & Drop**: Drag item from Components to Steps list
2. **Click to Add**: Click item → Added to end of steps
3. **Quick Create**: "+ New" button in each tab to create inline
4. **Edit Reference**: Click "Edit" to open item in dedicated editor (for complex items)

---

## 6. Settings Panel Behavior

### 6.1 Dynamic Settings

Settings panel shows only relevant options based on steps:

```typescript
function getVisibleSettings(features: WorkflowFeatures): SettingsConfig {
  return {
    // Always visible
    name: true,
    description: true,
    category: true,
    tags: true,

    // Orchestration settings
    goal: features.orchestration,
    maxIterations: features.orchestration,
    persistentSession: features.orchestration,

    // Verification settings
    verificationFirst: features.showVerificationFirst,

    // Config settings
    configSelector: features.requiresConfig,

    // AI provider settings
    provider: features.orchestration,
    model: features.orchestration,
  };
}
```

### 6.2 Settings Grouping

```
WORKFLOW SETTINGS
├── Basic
│   ├── Name
│   ├── Description
│   └── Category/Tags
│
├── AI Settings (when has Prompt)
│   ├── Goal
│   ├── Max Iterations
│   ├── Provider
│   ├── Model
│   └── Persistent Session
│
├── Verification (when has Tests)
│   └── Run Verification First
│
└── GUI Config (when has GUI Actions)
    └── Configuration Selector
```

---

## 7. Migration Strategy

### 7.1 Data Migration

**Existing Data → Unified Workflow:**

| Source | Migration |
|--------|-----------|
| `prompts` (Tasks) | → Workflow with single Prompt step |
| `ai-workflows` | → Workflow (steps already compatible) |
| `gui-workflows` | → Workflow with GUI Action steps |
| `playwright/scripts` | Keep as reference library |
| `verifications` | Keep as reference library |

### 7.2 API Changes

**New unified endpoint:**
```
GET/POST /workflows          # All workflows
GET/POST /workflows/:id
```

**Keep existing for components:**
```
GET/POST /scripts            # Playwright/Python scripts
GET/POST /tests              # Verification test definitions
GET/POST /prompts            # Prompt templates (optional)
```

### 7.3 Backward Compatibility

During migration period:
- Keep old endpoints working
- Add `workflow_type` field to distinguish legacy data
- Gradually migrate UI to unified builder
- Remove old builders after migration complete

---

## 8. Implementation Phases

### Phase 1: Unified Type System (2-3 days)
1. Create `UnifiedStep` type definition
2. Create `Workflow` type with auto-computed features
3. Add feature detection logic
4. Create step type icons/colors constants

### Phase 2: Unified Builder UI (4-5 days)
1. Create new `WorkflowBuilderTab` component
2. Implement step list with all step types
3. Implement step editor for each type
4. Implement dynamic settings panel
5. Implement Components panel with tabs

### Phase 3: Navigation Restructure (1-2 days)
1. Update Sidebar navigation
2. Remove old builder routes (AI Workflows, GUI Workflows, Scripts, Tests)
3. Update Library to use unified workflow display
4. Update routing in App.tsx

### Phase 4: Data Migration (2-3 days)
1. Create migration script for existing data
2. Update API endpoints
3. Update backend Rust types
4. Run migration on existing user data

### Phase 5: Polish & Edge Cases (2-3 days)
1. Handle empty workflows
2. Handle single-step workflows (task-like)
3. Keyboard shortcuts
4. Drag-and-drop reordering
5. Copy/paste steps

**Total Estimated Time**: 11-16 days

---

## 9. UI Mockups

### 9.1 Empty State

```
┌─────────────────────────────────────────────────────────────┐
│ New Workflow                                    [Save] [Run]│
├─────────────────────────────────────────────────────────────┤
│                                                             │
│                    📋 Start Building                        │
│                                                             │
│      Add steps to build your workflow. Steps run in order: │
│           Setup → Verification → Agentic (loop)            │
│                                                             │
│   ┌──────────┐  ┌──────────┐  ┌──────────┐                 │
│   │ 1.SETUP  │  │ 2.VERIFY │  │ 3.AGENTIC│                 │
│   │  Script  │  │   Test   │  │  Prompt  │                 │
│   └──────────┘  └──────────┘  └──────────┘                 │
│                                                             │
│                    [+ Add Step ▼]                           │
│                                                             │
└─────────────────────────────────────────────────────────────┘
```

### 9.2 Prompt-Only Workflow (Simple Task)

```
┌─────────────────────────────────────────────────────────────┐
│ Fix TypeScript Errors                           [Save] [Run]│
├─────────────────────────────────────────────────────────────┤
│                                                             │
│ STEPS                              SETTINGS                 │
│                                    ┌───────────────────────┐│
│ ▼ AGENTIC ─────────────────────    │ Max Iterations: [10] ││
│ ┌─────────────────────────────┐    │ Provider: [Claude ▼] ││
│ │ 💬 Fix all TS errors        │    │ Model: [Opus ▼]      ││
│ │    "Analyze the codebase... │    └───────────────────────┘│
│ └─────────────────────────────┘                             │
│ [+ Add Agentic Step]                                        │
│                                                             │
│ [+ Add Step ▼]                                              │
│                                                             │
└─────────────────────────────────────────────────────────────┘
```

### 9.3 Full Orchestrated Workflow (Collapsible Phase Sections)

```
┌─────────────────────────────────────────────────────────────┐
│ Improve All Code Quality                        [Save] [Run]│
├─────────────────────────────────────────────────────────────┤
│                                                             │
│ STEPS                              SETTINGS                 │
│                                    ┌───────────────────────┐│
│ ▶ SETUP (empty) ───────────────    │ Max Iterations: [10] ││
│                                    │ Provider: [Claude ▼] ││
│ ▼ VERIFICATION ────────────────    │ Model: [Opus ▼]      ││
│ ┌─────────────────────────────┐    └───────────────────────┘│
│ │ ✓ Type Check (critical)     │                             │
│ │   npm run typecheck         │                             │
│ ├─────────────────────────────┤                             │
│ │ ✓ Lint Check                │                             │
│ │   npm run lint              │                             │
│ ├─────────────────────────────┤                             │
│ │ ✓ Build Check (critical)    │                             │
│ │   npm run build             │                             │
│ └─────────────────────────────┘                             │
│ [+ Add Verification Step]                                   │
│                                                             │
│ ▼ AGENTIC ─────────────────────                             │
│ ┌─────────────────────────────┐                             │
│ │ 💬 Fix Issues               │                             │
│ │   "Fix all errors found..." │                             │
│ └─────────────────────────────┘                             │
│ [+ Add Agentic Step]                                        │
│                                                             │
│ [+ Add Step ▼]                                              │
│                                                             │
└─────────────────────────────────────────────────────────────┘
```

### 9.4 GUI Automation Workflow

```
┌─────────────────────────────────────────────────────────────┐
│ Login and Verify Dashboard                      [Save] [Run]│
├─────────────────────────────────────────────────────────────┤
│                                                             │
│ STEPS                              SETTINGS                 │
│                                    ┌───────────────────────┐│
│ ▼ SETUP ───────────────────────    │ Config: [app.json ▼] ││
│ ┌─────────────────────────────┐    └───────────────────────┘│
│ │ 📜 Login Script             │                             │
│ │   Navigate to login page    │                             │
│ ├─────────────────────────────┤                             │
│ │ 🖱️ Click "Dashboard" tab    │                             │
│ └─────────────────────────────┘                             │
│ [+ Add Setup Step]                                          │
│                                                             │
│ ▼ VERIFICATION ────────────────                             │
│ ┌─────────────────────────────┐                             │
│ │ 📷 Screenshot               │                             │
│ │   Capture dashboard state   │                             │
│ ├─────────────────────────────┤                             │
│ │ 🎭 Dashboard Elements Test  │                             │
│ │   Check widgets loaded      │                             │
│ └─────────────────────────────┘                             │
│ [+ Add Verification Step]                                   │
│                                                             │
│ ▶ AGENTIC (empty) ─────────────                             │
│                                                             │
│ [+ Add Step ▼]                                              │
│                                                             │
└─────────────────────────────────────────────────────────────┘
```

### 9.5 Phase Section Behavior

| State | Display |
|-------|---------|
| **Empty phase** | Collapsed, shows "(empty)" label, click to expand |
| **Phase with steps** | Expanded by default, collapsible |
| **All phases empty** | Show helpful empty state (9.1) |

**Interaction:**
- Click phase header to expand/collapse
- Drag steps to reorder within phase (not across phases)
- Each phase has its own "+ Add Step" button filtered to that phase's step types

---

## 10. Resolved Questions

1. **Dropdown organization** → Organized by phase (Setup, Verification, Agentic)
2. **GUI Actions placement** → Duplicated in both Setup and Verification
3. **Step reordering** → Locked within phase groups
4. **Phase display** → Collapsible sections in step list
5. **Scripts vs Tests** → Scripts are Playwright-only for setup; Tests are multi-type for verification
6. **Capture placement** → Capture (Screenshot) is a Verification activity
7. **Scripts and Tests editing** → Keep dedicated `/script-builder` and `/test-builder` pages (too complex for modals)
8. **GUI Action target selection** → Keep existing UI from AI Automation Builder
9. **Empty phase behavior** → Show all three phases always (clear when empty)
10. **Setup execution frequency**:
    - Qontinui automation (GUI Actions, State Navigation, Workflows): once by default, configurable
    - Playwright Scripts: every iteration (browser state not preserved)
11. **Playwright Test + Script fusion** → Configuration required:
    - `fused_script_id`: which script to fuse with
    - `execution_mode`: 'independent' (fresh session) or 'chained' (continue after previous test)

## 11. Open Questions

All major design questions have been resolved. Implementation can proceed.

---

## 12. Key Files to Modify

| File | Changes |
|------|---------|
| `src/types/workflow.ts` | New unified types |
| `src/components/workflow-builder/*` | New builder component |
| `src/components/navigation/Sidebar.tsx` | Updated navigation |
| `src/App.tsx` | Updated routing |
| `src/components/LibraryTab.tsx` | Updated to show unified workflows |
| `src-tauri/src/workflows.rs` | New unified backend |
| `src-tauri/src/mcp_api.rs` | New API endpoints |

---

*Document generated: 2026-01-11*
*Author: Claude Code*
