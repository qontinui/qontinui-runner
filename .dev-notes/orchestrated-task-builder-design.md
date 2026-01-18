# Orchestrated Task Builder Design Document

## Executive Summary

This document describes the design for extending the AI Automation Builder to support **Orchestrated Tasks** - a middle ground between Simple Tasks and AI Workflows that adds planning and verification loops to single-prompt tasks.

---

## 1. Current Architecture Analysis

### 1.1 Existing Data Models

**SavedPrompt (prompts.rs, lines 7-38)**
```rust
pub struct SavedPrompt {
    pub id: String,
    pub name: String,
    pub description: String,
    pub content: String,                          // The task prompt
    pub category: String,
    pub tags: Vec<String>,
    pub max_sessions: Option<u32>,
    pub provider: Option<String>,                 // AI provider override
    pub model: Option<String>,                    // Model override
    // Orchestrator fields (already exist but unused in UI)
    pub requires_orchestrator: Option<bool>,
    pub orchestrator_goal: Option<String>,
    pub orchestrator_max_iterations: Option<u32>,
    pub orchestrator_verification_first: Option<bool>,
    pub created_at: String,
    pub modified_at: String,
}
```

**Key Insight**: The orchestrator fields already exist in the data model but have no UI support.

### 1.2 Orchestrator System Components

The orchestrator system is well-implemented in Rust with these key components:

1. **Planning Agent** (`orchestrator/planning.rs`)
   - Analyzes goals and creates verification plans
   - Generates success criteria with deterministic and AI-evaluated checks
   - Detects project environment (npm, cargo, typescript, playwright)

2. **Verification Methods** (`orchestrator/types.rs`, lines 163-175)
   ```rust
   pub enum VerificationMethod {
       BuildSuccess,      // Build without errors
       UnitTest,          // Run unit tests
       IntegrationTest,   // Run integration tests
       Playwright,        // Run Playwright tests
       LogPattern,        // Check log patterns
       GuiAutomation,     // GUI automation checks
       TypeCheck,         // Type checking (tsc, mypy)
       LintCheck,         // Linting
       CustomCommand,     // Custom shell command
   }
   ```

3. **Success Criteria** (`orchestrator/types.rs`, lines 130-160)
   - `is_critical: bool` - Whether failure blocks completion
   - `criterion_type: CriterionType` - Deterministic vs AI-evaluated
   - `verification_config: Option<Value>` - Config for the check

### 1.3 Current UI Structure

The AI Builder uses a modular component architecture in `src/components/ai-builder/`:

```
ai-builder/
  index.tsx              - Main entry with AiBuilderProvider
  AiBuilderContext.tsx   - State management context
  AiBuilderContent.tsx   - Main layout composition
  types.ts               - TypeScript interfaces
  Header.tsx             - Title and mode selector
  ExecutionStepsList.tsx - Step editor for workflows
  SettingsPanel.tsx      - Provider/model/iterations config
  AdvancedSettingsPanel.tsx - Advanced options
  SavedWorkflowsPanel.tsx   - Workflow library
  ...
```

---

## 2. Design: Three Task Types

### 2.1 Task Type Taxonomy

| Type | Description | Orchestration | Execution Steps | Verification |
|------|-------------|---------------|-----------------|--------------|
| **Simple Task** | Single prompt, runs until TASK_COMPLETE | None | None | None |
| **Orchestrated Task** | Single prompt + planning + verification loop | Full orchestrator | None (optional) | Yes |
| **AI Workflow** | Multi-step sequence | Optional | Yes (required) | Optional |

### 2.2 Key Differences

**Simple Task (Current)**
- User writes prompt
- Session runs until `[TASK_COMPLETE]` signal
- No structured verification

**Orchestrated Task (New)**
- User writes prompt + goal
- Planning Agent creates verification criteria
- Worker runs prompt
- Verification Agent checks success criteria
- Loop continues until all criteria pass or max iterations

**AI Workflow (Current)**
- User defines execution steps (screenshots, clicks, playwright)
- Steps execute deterministically
- AI analyzes results
- Optional verification loop

---

## 3. UI Flow Design

### 3.1 Task Type Selection

Add a task type selector to the Header component when creating a new task.

**Location**: In `Header.tsx` or a new `TaskTypeSelector.tsx` component

**Design**:
```
+------------------------------------------+
|  Create New Task                         |
+------------------------------------------+
|  [Simple Task]  [Orchestrated]  [Multi-Step] |
|      ^                                    |
|  Single prompt    Planning +      Execution  |
|  No verification  Verification    Steps      |
+------------------------------------------+
```

When user selects a type, the UI transforms:
- **Simple Task**: Shows prompt editor + basic settings
- **Orchestrated**: Shows prompt editor + goal + verification criteria builder
- **Multi-Step**: Shows current workflow builder with execution steps

### 3.2 Orchestrated Task Form

When "Orchestrated Task" is selected, display:

```
+--------------------------------------------------+
| TASK NAME                                         |
| [________________________]                        |
|                                                   |
| GOAL (What should be accomplished)               |
| [________________________]                        |
| This defines success for the verification system  |
|                                                   |
| TASK PROMPT                                       |
| [________________________]                        |
| [________________________]                        |
| Instructions for the AI worker                    |
|                                                   |
| VERIFICATION CRITERIA                             |
| +----------------------------------------------+ |
| | [ ] Build must pass                          | |
| |     [ ] Critical                             | |
| +----------------------------------------------+ |
| | [ ] Type check must pass                     | |
| |     Command: [npm run typecheck]             | |
| |     [ ] Critical                             | |
| +----------------------------------------------+ |
| | [ ] Lint check must pass                     | |
| |     Command: [npm run lint]                  | |
| |     [ ] Non-critical (informational)         | |
| +----------------------------------------------+ |
| | [+ Add Criterion]                            | |
| +----------------------------------------------+ |
|                                                   |
| TEMPLATES                                         |
| [Python Quality] [TypeScript Build] [Rust CI]     |
|                                                   |
| SETTINGS                                          |
| Max iterations: [10]  Run verification first: [ ] |
+--------------------------------------------------+
```

### 3.3 Verification Criteria Builder

**Component**: `VerificationCriteriaBuilder.tsx`

**Functionality**:
1. List of criteria (add/remove/reorder)
2. Each criterion has:
   - Type selector (deterministic methods + AI-evaluated)
   - Configuration fields based on type
   - Critical/non-critical toggle
   - Description field

**Deterministic Criterion Types**:

| Type | Config Fields | Example |
|------|---------------|---------|
| BuildSuccess | command (optional) | `npm run build` |
| TypeCheck | command | `npm run typecheck` |
| LintCheck | command | `npm run lint` |
| UnitTest | test_pattern, command | `npm test -- --grep "login"` |
| Playwright | script_id or inline | Select from saved scripts |
| CustomCommand | command, expected_exit | Any shell command |
| LogPattern | pattern, log_path, should_match | Regex in log file |

**AI-Evaluated Criterion**:
- Description of what to evaluate
- Prompt for the verification agent
- Screenshot reference (optional)

### 3.4 Verification Templates

Pre-defined criterion sets for common scenarios:

**Python Quality Template**:
```json
{
  "name": "Python Quality",
  "criteria": [
    { "type": "custom_command", "command": "poetry run black --check .", "description": "Code formatting", "is_critical": false },
    { "type": "custom_command", "command": "poetry run ruff check .", "description": "Linting", "is_critical": true },
    { "type": "custom_command", "command": "poetry run mypy .", "description": "Type checking", "is_critical": true },
    { "type": "custom_command", "command": "poetry run pytest", "description": "Tests pass", "is_critical": true }
  ]
}
```

**TypeScript Build Template**:
```json
{
  "name": "TypeScript Build",
  "criteria": [
    { "type": "type_check", "command": "npm run typecheck", "is_critical": true },
    { "type": "lint_check", "command": "npm run lint", "is_critical": false },
    { "type": "build_success", "command": "npm run build", "is_critical": true }
  ]
}
```

**Rust CI Template**:
```json
{
  "name": "Rust CI",
  "criteria": [
    { "type": "custom_command", "command": "cargo fmt --check", "is_critical": false },
    { "type": "custom_command", "command": "cargo clippy -- -D warnings", "is_critical": true },
    { "type": "build_success", "command": "cargo build", "is_critical": true },
    { "type": "unit_test", "command": "cargo test", "is_critical": true }
  ]
}
```

---

## 4. Component Structure

### 4.1 New Components to Create

```
src/components/ai-builder/
  TaskTypeSelector.tsx          - Radio buttons for task type
  OrchestratedTaskForm.tsx      - Form for orchestrated tasks
  VerificationCriteriaBuilder.tsx - Criteria list editor
  VerificationCriterionItem.tsx   - Single criterion editor
  VerificationTemplatesDropdown.tsx - Template selector
  constants/
    verification-templates.ts   - Predefined templates
```

### 4.2 Component Responsibilities

**TaskTypeSelector.tsx**
- Displays three task type options
- Emits selection change event
- Shows brief description for each type

**OrchestratedTaskForm.tsx**
- Goal input field
- Prompt editor (reuse existing)
- Embeds VerificationCriteriaBuilder
- Template quick-apply buttons
- Settings (max_iterations, verification_first toggle)

**VerificationCriteriaBuilder.tsx**
- Manages list of criteria
- Add/remove/reorder functionality
- Maps to `SuccessCriterion[]` type

**VerificationCriterionItem.tsx**
- Type selector dropdown
- Dynamic config fields based on type
- Critical toggle
- Delete button

### 4.3 Integration Points

**AiBuilderContext.tsx** - Add new state:
```typescript
interface AiBuilderState {
  // Existing...
  taskType: 'simple' | 'orchestrated' | 'workflow';
  orchestratorGoal: string;
  verificationCriteria: SuccessCriterion[];
  verificationFirst: boolean;
}
```

**SavedWorkflowsPanel.tsx** - Update to show task type badges

**LibrarySubTab.tsx** - Update to display orchestrated tasks with their criteria

---

## 5. Data Flow

### 5.1 Saving an Orchestrated Task

1. User fills in form (name, goal, prompt, criteria)
2. Frontend builds `SavedPrompt` with orchestrator fields:
   ```typescript
   {
     name: "Fix Login Validation",
     description: "Add proper validation to login form",
     content: "Implement email validation...",
     requires_orchestrator: true,
     orchestrator_goal: "Login form validates email and shows errors",
     orchestrator_max_iterations: 10,
     orchestrator_verification_first: false,
     // Criteria stored as JSON in a new field or separate table
     verification_criteria: [...]
   }
   ```
3. POST to `/prompts` endpoint
4. Backend stores in `prompts.json`

### 5.2 Running an Orchestrated Task

1. User clicks "Run" on task in Library
2. Frontend calls `POST /sessions/start` with:
   ```json
   {
     "name": "Fix Login Validation",
     "prompt": "...",
     "orchestrator_config": {
       "enabled": true,
       "goal": "Login form validates email and shows errors",
       "max_iterations": 10,
       "verification_first": false,
       "criteria": [...]
     }
   }
   ```
3. Backend initializes orchestrator
4. Planning Agent creates verification plan (or uses provided criteria)
5. Worker executes prompt
6. Verification Agent checks criteria
7. Loop until success or max iterations

### 5.3 API Changes

**New endpoint fields for `/prompts`**:
```typescript
interface SavedPromptRequest {
  // Existing fields...
  requires_orchestrator?: boolean;
  orchestrator_goal?: string;
  orchestrator_max_iterations?: number;
  orchestrator_verification_first?: boolean;
  verification_criteria?: SuccessCriterion[];  // NEW
}
```

**Session start modifications**:
```typescript
interface SessionStartRequest {
  // Existing fields...
  orchestrator_config?: {
    enabled: boolean;
    goal: string;
    max_iterations: number;
    verification_first: boolean;
    criteria: SuccessCriterion[];
  };
}
```

---

## 6. Implementation Phases

### Phase 1: Task Type UI Foundation (2-3 days)
1. Create `TaskTypeSelector.tsx` component
2. Add `taskType` to `AiBuilderContext`
3. Modify `AiBuilderContent.tsx` to render different forms based on type
4. Ensure existing workflow functionality is preserved

### Phase 2: Orchestrated Task Form (3-4 days)
1. Create `OrchestratedTaskForm.tsx`
2. Create `VerificationCriteriaBuilder.tsx`
3. Create `VerificationCriterionItem.tsx`
4. Implement dynamic config fields for each criterion type

### Phase 3: Templates and UX (1-2 days)
1. Create `verification-templates.ts` with predefined templates
2. Create `VerificationTemplatesDropdown.tsx`
3. Add project detection to auto-suggest templates

### Phase 4: Backend Integration (2-3 days)
1. Add `verification_criteria` field to `SavedPrompt` in Rust
2. Update prompts API to handle criteria
3. Modify session start to accept orchestrator config
4. Wire up existing orchestrator code to use UI-provided criteria

### Phase 5: Library Display (1-2 days)
1. Update `LibrarySubTab.tsx` to show task types
2. Add criteria preview in expanded view
3. Add visual indicators for orchestrated tasks

### Phase 6: Testing and Polish (2 days)
1. Test full flow: create, save, run, verify
2. Handle edge cases (empty criteria, invalid commands)
3. Add loading states and error handling

**Total Estimated Time**: 11-16 days

---

## 7. Key Design Decisions

### 7.1 Criteria Storage Strategy

**Option A**: Store criteria as JSON in `SavedPrompt.verification_criteria`
- Pros: Simple, self-contained
- Cons: No validation, harder to query

**Option B**: Separate criteria table with foreign key to prompts
- Pros: Better validation, queryable
- Cons: More complex, additional migrations

**Recommendation**: Start with Option A for simplicity, migrate to Option B if needed.

### 7.2 Planning Agent Role

**Option A**: Always use Planning Agent to generate criteria from goal
- Pros: Intelligent decomposition, adapts to context
- Cons: AI cost, latency, unpredictable output

**Option B**: Use user-defined criteria, skip planning if criteria provided
- Pros: Deterministic, fast, user control
- Cons: Requires user to define all criteria

**Option C**: Hybrid - use Planning Agent to suggest, user can modify
- Pros: Best of both worlds
- Cons: Most complex to implement

**Recommendation**: Start with Option B (user-defined criteria), add Option C as enhancement.

### 7.3 Backward Compatibility

The existing orchestrator fields in `SavedPrompt` are optional and unused. The new implementation should:
1. Use the existing fields for basic config
2. Add a new `verification_criteria` field for criteria
3. Tasks without criteria continue to work as simple tasks

---

## 8. Critical Files for Implementation

| File | Purpose |
|------|---------|
| `src/components/ai-builder/AiBuilderContext.tsx` | Core state management - add task type, orchestrator goal, verification criteria |
| `src/components/ai-builder/AiBuilderContent.tsx` | Main layout - conditionally render forms based on task type |
| `src-tauri/src/prompts.rs` | Backend SavedPrompt model - add verification_criteria field |
| `src-tauri/src/orchestrator/types.rs` | Defines SuccessCriterion, VerificationMethod - frontend must align with these |
| `src/components/ai-workflows/LibrarySubTab.tsx` | Library display - show task type badges and criteria previews |

---

## 9. Open Questions

1. **Should criteria be editable after task creation?**
   - Recommendation: Yes, via the edit dialog

2. **How to handle criteria that reference Playwright scripts?**
   - Show a dropdown of saved scripts, store script_id

3. **Should we support criterion templates at the individual level?**
   - e.g., "Add TypeCheck criterion" button that pre-fills common settings

4. **How to visualize verification results in the Active Dashboard?**
   - Show pass/fail status for each criterion during execution

---

*Document generated: 2026-01-11*
*Author: Planning Agent*
