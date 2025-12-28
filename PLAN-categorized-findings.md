# Categorized Findings & Execution Reports

## Overview

Transform the current issue tracking into a comprehensive "Findings" system that:
1. Categorizes detected items appropriately (not everything is a bug)
2. Provides categorized execution reports
3. Supports user-defined categories
4. Enables AI-user communication for complex items

---

## Phase 1: Enhanced Finding Categories

### New Data Model

```typescript
// Built-in category IDs
type BuiltInCategoryId =
  | 'code_bug'           // Actual code issues - auto-fixable
  | 'todo'               // TODOs - may need user input
  | 'config_issue'       // Configuration/environment problems
  | 'already_fixed'      // Resolved in previous sessions
  | 'expected_behavior'  // By design, not a bug
  | 'data_migration'     // Requires admin intervention
  | 'runtime_issue'      // Operational issues, not code
  | 'test_issue'         // Problems with test code
  | 'enhancement'        // Improvement suggestions
  | 'warning'            // Things to be aware of
  | 'documentation'      // Doc issues/improvements
  | 'security'           // Security concerns
  | 'performance';       // Performance issues

interface FindingCategory {
  id: string;
  name: string;
  description: string;
  icon: string;              // Lucide icon name
  color: string;             // Tailwind color class
  isBuiltIn: boolean;
  defaultActionType: ActionType;
  sortOrder: number;
}

type ActionType =
  | 'auto_fix'           // Can be fixed by AI without user input
  | 'needs_user_input'   // AI needs to ask questions first
  | 'manual'             // User must handle manually
  | 'informational';     // No action needed, just FYI

interface Finding {
  id: string;
  categoryId: string;
  severity: 'critical' | 'high' | 'medium' | 'low' | 'info';
  status: FindingStatus;
  title: string;
  description: string;

  // Source tracking
  sourceSessionId?: string;
  sourcePromptName?: string;
  detectedAt: number;

  // Action handling
  actionType: ActionType;
  actionable: boolean;

  // For items needing user input
  pendingQuestion?: UserInputRequest;
  userResponse?: string;

  // Resolution
  resolvedAt?: number;
  resolution?: string;

  // Context for AI
  codeContext?: {
    file?: string;
    line?: number;
    snippet?: string;
  };

  metadata?: Record<string, unknown>;
}

type FindingStatus =
  | 'detected'           // Just found
  | 'in_progress'        // Being worked on
  | 'needs_input'        // Waiting for user input
  | 'resolved'           // Fixed/handled
  | 'wont_fix'           // Intentionally not fixing
  | 'deferred';          // Will handle later

interface UserInputRequest {
  id: string;
  findingId: string;
  question: string;
  context?: string;       // Additional context for the user
  inputType: 'text' | 'choice' | 'boolean' | 'code';
  options?: Array<{
    value: string;
    label: string;
    description?: string;
  }>;
  required: boolean;
  defaultValue?: string;
}
```

### Built-in Categories Configuration

```typescript
const BUILT_IN_CATEGORIES: FindingCategory[] = [
  {
    id: 'code_bug',
    name: 'Code Bug',
    description: 'Actual code issues that can be auto-fixed',
    icon: 'Bug',
    color: 'red',
    isBuiltIn: true,
    defaultActionType: 'auto_fix',
    sortOrder: 1
  },
  {
    id: 'todo',
    name: 'TODO',
    description: 'Tasks that may require user decisions',
    icon: 'CheckSquare',
    color: 'amber',
    isBuiltIn: true,
    defaultActionType: 'needs_user_input',
    sortOrder: 2
  },
  {
    id: 'security',
    name: 'Security',
    description: 'Security vulnerabilities or concerns',
    icon: 'Shield',
    color: 'red',
    isBuiltIn: true,
    defaultActionType: 'auto_fix',
    sortOrder: 3
  },
  {
    id: 'config_issue',
    name: 'Configuration Issue',
    description: 'Configuration or environment problems',
    icon: 'Settings',
    color: 'orange',
    isBuiltIn: true,
    defaultActionType: 'manual',
    sortOrder: 4
  },
  {
    id: 'already_fixed',
    name: 'Already Fixed',
    description: 'Issues resolved in previous sessions',
    icon: 'CheckCircle',
    color: 'green',
    isBuiltIn: true,
    defaultActionType: 'informational',
    sortOrder: 5
  },
  {
    id: 'expected_behavior',
    name: 'Expected Behavior',
    description: 'Intentional design, not a bug',
    icon: 'Info',
    color: 'blue',
    isBuiltIn: true,
    defaultActionType: 'informational',
    sortOrder: 6
  },
  {
    id: 'data_migration',
    name: 'Data Migration',
    description: 'Requires admin or manual intervention',
    icon: 'Database',
    color: 'purple',
    isBuiltIn: true,
    defaultActionType: 'manual',
    sortOrder: 7
  },
  {
    id: 'runtime_issue',
    name: 'Runtime Issue',
    description: 'Operational issues, not code bugs',
    icon: 'Activity',
    color: 'yellow',
    isBuiltIn: true,
    defaultActionType: 'manual',
    sortOrder: 8
  },
  {
    id: 'test_issue',
    name: 'Test Issue',
    description: 'Problems with test code or test setup',
    icon: 'TestTube',
    color: 'purple',
    isBuiltIn: true,
    defaultActionType: 'auto_fix',
    sortOrder: 9
  },
  {
    id: 'enhancement',
    name: 'Enhancement',
    description: 'Improvement suggestions',
    icon: 'Sparkles',
    color: 'cyan',
    isBuiltIn: true,
    defaultActionType: 'needs_user_input',
    sortOrder: 10
  },
  {
    id: 'documentation',
    name: 'Documentation',
    description: 'Documentation issues or improvements',
    icon: 'FileText',
    color: 'slate',
    isBuiltIn: true,
    defaultActionType: 'auto_fix',
    sortOrder: 11
  },
  {
    id: 'performance',
    name: 'Performance',
    description: 'Performance issues or optimization opportunities',
    icon: 'Zap',
    color: 'yellow',
    isBuiltIn: true,
    defaultActionType: 'needs_user_input',
    sortOrder: 12
  },
  {
    id: 'warning',
    name: 'Warning',
    description: 'Things to be aware of',
    icon: 'AlertTriangle',
    color: 'yellow',
    isBuiltIn: true,
    defaultActionType: 'informational',
    sortOrder: 13
  }
];
```

---

## Phase 2: Execution Reports

### Report Data Model

```typescript
interface ExecutionReport {
  id: string;
  sessionId: string;
  promptName: string;
  promptId?: string;

  // Timing
  startedAt: number;
  completedAt?: number;
  duration?: number;

  // Status
  status: 'running' | 'completed' | 'paused_for_input' | 'failed' | 'cancelled';

  // Findings organized by category
  findings: Finding[];

  // Summary statistics
  summary: ReportSummary;

  // Pending user inputs (when paused)
  pendingInputs: UserInputRequest[];

  // Phases/iterations
  phases: PhaseInfo[];
}

interface ReportSummary {
  totalFindings: number;

  byCategory: Record<string, {
    count: number;
    actionable: number;
    resolved: number;
  }>;

  bySeverity: Record<string, number>;

  byStatus: Record<string, number>;

  actionable: number;
  needsUserInput: number;
  autoFixed: number;
  informational: number;
}

interface PhaseInfo {
  phase: number;
  startedAt: number;
  completedAt?: number;
  findingsDetected: number;
  findingsResolved: number;
}
```

### Report UI Components

```
ExecutionReportView
├── ReportHeader (prompt name, timing, overall status)
├── ReportSummary (pie chart or stats cards)
├── CategoryTabs or Accordion
│   ├── CategorySection (Code Bugs)
│   │   ├── FindingCard (with "Analyze & Fix" button)
│   │   └── ...
│   ├── CategorySection (TODOs)
│   │   ├── FindingCard (with "Provide Input" button)
│   │   └── ...
│   └── CategorySection (Informational)
│       ├── FindingCard (no action, just info)
│       └── ...
└── PendingInputsPanel (if any questions need answers)
```

---

## Phase 3: AI-User Communication Flow

### How It Works (Using Existing Checkpoint System)

```
┌─────────────────────────────────────────────────────────────┐
│ 1. AI Execution Starts                                       │
│    - User runs "Improve-All" prompt                         │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│ 2. AI Detects Findings                                       │
│    - Categorizes each finding                               │
│    - For TODOs/complex items, creates UserInputRequest      │
│    - Writes to checkpoint: custom_data.pendingInputs = [...]│
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│ 3. Phase Completes with Pending Inputs                       │
│    - Session status: "waiting_for_continuation"             │
│    - UI shows: "X items need your input before continuing"  │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│ 4. User Answers Questions                                    │
│    - UI presents questions in a friendly format             │
│    - User provides responses (text, choices, etc.)          │
│    - Responses saved to checkpoint                          │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│ 5. User Clicks "Continue"                                    │
│    - Continuation prompt includes user's answers            │
│    - AI receives context and continues execution            │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│ 6. AI Handles Items with User Input                          │
│    - Uses provided answers to make decisions                │
│    - Can auto-fix items that now have sufficient info       │
└─────────────────────────────────────────────────────────────┘
```

### Continuation Prompt Enhancement

Current continuation prompt:
```
Continue the workflow from phase X...
```

Enhanced continuation prompt:
```
Continue the workflow from phase X.

User has provided the following responses to your questions:

## TODO: Implement caching strategy
**Question:** Should caching use Redis or in-memory store?
**User Response:** Use Redis for production, in-memory for development

## TODO: Add rate limiting
**Question:** What rate limit should be applied?
**User Response:** 100 requests per minute per user

Please proceed with implementing these items using the provided context.
```

---

## Phase 4: Custom Categories

### Category Management

```typescript
// Storage: localStorage or backend API
interface CategoryStore {
  customCategories: FindingCategory[];
  categoryOrder: string[]; // All category IDs in display order
}

// API endpoints (if using backend)
// GET /api/finding-categories
// POST /api/finding-categories
// PUT /api/finding-categories/:id
// DELETE /api/finding-categories/:id
```

### Category Management UI

Located in: Configure > Findings Settings (new tab)

Features:
- View all categories (built-in + custom)
- Add custom category
- Edit custom category (name, icon, color, default action)
- Reorder categories
- Cannot delete built-in categories
- Can hide/disable categories

---

## Phase 5: Enhanced AI Detection

### Updated Prompt Instructions

The Improve-All and similar prompts should be updated to:

1. **Categorize findings** using structured output:
```
[FINDING:code_bug:high]
Title: Null pointer exception in UserService
Description: ...
File: src/services/UserService.ts:42
[/FINDING]

[FINDING:todo:medium:needs_input]
Title: Implement user notification preferences
Description: The TODO mentions adding notification preferences
Question: What notification channels should be supported?
Options: Email only | Email + SMS | Email + SMS + Push
[/FINDING]

[FINDING:already_fixed:info]
Title: Missing type annotations
Description: This was fixed in the previous session
Resolution: Types added in commit abc123
[/FINDING]
```

2. **Detect and parse structured findings** in the FindingsTracker service

---

## Implementation Order

### Sprint 1: Core Types & Detection (2-3 days)
- [ ] Create Finding and FindingCategory types
- [ ] Create built-in categories
- [ ] Update/create FindingsTracker service
- [ ] Update AI output parsing to detect categorized findings

### Sprint 2: Execution Reports UI (2-3 days)
- [ ] Create ExecutionReport component
- [ ] Add report view to Monitor tab
- [ ] Category-based grouping and filtering
- [ ] Summary statistics

### Sprint 3: User Input Flow (3-4 days)
- [ ] Add pendingInputs to checkpoint system
- [ ] Create UserInputPanel component
- [ ] Update continuation prompt to include answers
- [ ] Test end-to-end flow

### Sprint 4: Category Management (1-2 days)
- [ ] Create category management UI
- [ ] Custom category CRUD
- [ ] Category ordering

### Sprint 5: Polish & Integration (2 days)
- [ ] Update Improve-All prompt for structured output
- [ ] Add category-specific action buttons
- [ ] Testing and refinement

---

## Files to Create/Modify

### New Files
- `src/types/findings.ts` - Finding types
- `src/services/FindingsTracker.ts` - Findings management
- `src/services/FindingCategories.ts` - Category management
- `src/components/findings/ExecutionReport.tsx` - Report view
- `src/components/findings/FindingCard.tsx` - Individual finding
- `src/components/findings/CategorySection.tsx` - Category group
- `src/components/findings/UserInputPanel.tsx` - Question answering
- `src/components/findings/CategoryManager.tsx` - Category settings

### Modify
- `src/types/issues.ts` - Migrate to new Finding type
- `src/services/IssueTracker.ts` - Migrate or deprecate
- `src/components/ai-workflows/MonitorSubTab.tsx` - Add reports view
- `src/components/ai-workflows/SessionSubTab.tsx` - Show pending inputs
- Prompt templates - Add structured finding output

---

## Questions for User

1. Should custom categories be stored locally (localStorage) or synced to the web backend?

2. For the "Analyze & Fix" action on different categories, should there be category-specific prompts?

3. Should findings persist across runner restarts, or only for the current session?

4. Should there be a "bulk actions" feature (e.g., mark all "Already Fixed" as resolved)?
