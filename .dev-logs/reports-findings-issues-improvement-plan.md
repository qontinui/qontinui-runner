# Reports, Findings, and Issues - Architecture Improvement Plan

## Executive Summary

The qontinui-runner has evolved organically to include multiple overlapping systems for tracking AI workflow outputs. This analysis identifies critical issues including SRP violations, dead code, duplicate processing, and missing functionality. The improvement plan proposes a unified architecture with clear separation of concerns.

---

## Current Architecture Analysis

### 1. Identified Systems

| System | Purpose | Status |
|--------|---------|--------|
| **IssueTracker** | Legacy issue detection with `[ISSUE:*]` markers | Active but legacy |
| **FindingsTracker** | Categorized findings with `[FINDING:*:*]` markers | Current, primary |
| **ExecutionReportingService** | Action-level execution reporting | Active |
| **TestRunReportingService** | Legacy test run reporting | Deprecated |

### 2. Data Flow

```
AI Output Stream (Rust → Tauri Event)
        ↓
aiOutputHandlers.ts (line 44-84)
        ├→ logManager.addAiOutputLog() [display]
        ├→ issueTracker.processLine() [legacy]
        └→ findingsTracker.processLine() [current]
              ↓
       [In-Memory Storage]
              ↓
       [No Backend Sync] ← PROBLEM
```

---

## Critical Issues Found

### Issue 1: Dead Code - Backend Sync Never Called

**Location:** `src/findings/FindingsSync.ts` lines 148-249

**Problem:** The `syncFindingsToBackend()` and `syncReportToBackend()` functions are defined and exported but **never called anywhere** in the codebase.

```typescript
// Defined but never invoked:
export async function syncFindingsToBackend(findings, options): Promise<SyncResult>
export async function syncReportToBackend(report, options): Promise<SyncResult>
```

**Impact:** Reports and findings are never persisted to the backend, only held in memory and local files.

---

### Issue 2: Duplicate Processing

**Location:** `src/managers/event-handlers/aiOutputHandlers.ts` lines 79-84

**Problem:** Every AI output line is processed by BOTH trackers:

```typescript
if (source === "claude" || source === "ai") {
  issueTracker.processLine(line);    // Legacy
  findingsTracker.processLine(line); // Current
}
```

**Impact:**
- Redundant processing overhead
- Confusing user experience with two different tracking systems
- Inconsistent data between systems

---

### Issue 3: SRP Violations

#### 3.1 FindingsTracker.ts (~800+ lines)
**Location:** `src/services/FindingsTracker.ts`

Single class handles:
- Line parsing and regex matching
- Multi-line buffer management
- Finding CRUD operations
- Report lifecycle (create/update/complete)
- Session management
- Event emission
- Persistence coordination
- User input handling

#### 3.2 Database Module (1752 lines)
**Location:** `src-tauri/src/database/mod.rs`

Single module handles:
- Connection pooling
- 5+ different table schemas
- Task runs, sessions, checkpoints, settings, configs, scheduler
- Migrations
- JSON file migration

---

### Issue 4: Inconsistent Persistence Model

| Data Type | Memory | Local File | Backend | Notes |
|-----------|--------|------------|---------|-------|
| Findings | Yes | Yes (history) | **NO** | Sync code exists but not called |
| Issues | Yes | No | Yes | Via IssueSyncService |
| Reports | Yes | Partial | **NO** | Sync code exists but not called |
| Actions | Batched | No | Yes | Via ExecutionReportingService |

---

### Issue 5: Unclear Report Creation Flow

**Problem:** Reports are created but lifecycle is fragmented:

1. `aiOutputHandlers.ts` creates report on session start (line 71)
2. `FindingsTracker` updates report as findings detected
3. `executionHandlers.ts` calls `archiveCurrentSession()` on completion (line 190)
4. **No actual sync to backend occurs**

---

### Issue 6: Session ID Confusion

Multiple systems track their own session IDs:
- `IssueTracker.currentSessionId`
- `FindingsTracker.currentSessionId`
- `aiOutputHandlers.currentAiActionId`
- `ExecutionReportingService.activeRunId`

These may drift out of sync.

---

## Proposed Architecture

### New Layered Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                        AI OUTPUT STREAM                         │
└─────────────────────────────────────────────────────────────────┘
                              ↓
┌─────────────────────────────────────────────────────────────────┐
│                    UNIFIED AI OUTPUT HANDLER                    │
│  - Session management (single source of truth)                  │
│  - Line routing to appropriate parsers                          │
└─────────────────────────────────────────────────────────────────┘
                              ↓
┌──────────────────┬──────────────────┬──────────────────────────┐
│  FINDINGS PARSER │  ISSUE PARSER    │  ACTION TRACKER          │
│  (structured)    │  (legacy compat) │  (execution flow)        │
└──────────────────┴──────────────────┴──────────────────────────┘
                              ↓
┌─────────────────────────────────────────────────────────────────┐
│                     UNIFIED REPORT SERVICE                       │
│  - Aggregates findings, issues, and actions                     │
│  - Manages report lifecycle                                     │
│  - Handles user input requests                                  │
└─────────────────────────────────────────────────────────────────┘
                              ↓
┌─────────────────────────────────────────────────────────────────┐
│                    PERSISTENCE LAYER                            │
│  ┌────────────┐  ┌────────────┐  ┌────────────┐                │
│  │ Local File │  │  Database  │  │  Backend   │                │
│  │ (history)  │  │  (SQLite)  │  │  (API)     │                │
│  └────────────┘  └────────────┘  └────────────┘                │
└─────────────────────────────────────────────────────────────────┘
```

### Component Breakdown

#### 1. SessionManager (NEW)
Single source of truth for session lifecycle:

```typescript
interface SessionManager {
  // Session lifecycle
  startSession(actionId: string, name?: string): SessionContext;
  getCurrentSession(): SessionContext | null;
  endSession(status: SessionStatus): void;

  // Subscription
  onSessionChange(callback): Unsubscribe;
}

interface SessionContext {
  id: string;
  actionId: string;
  name: string;
  startedAt: number;
  status: SessionStatus;
}
```

#### 2. FindingsParser (Refactored)
Pure parsing, no state management:

```typescript
interface FindingsParser {
  // Parse single line, return finding if complete
  processLine(line: string): ParsedFinding | null;

  // Get parsing state (for debugging)
  getParsingState(): ParsingState;

  // Reset buffer
  reset(): void;
}
```

#### 3. IssueParser (Keep for Legacy)
Minimal legacy compatibility:

```typescript
interface IssueParser {
  processLine(line: string): ParsedIssue | null;
}
```

#### 4. UnifiedReportService (NEW)
Central report management:

```typescript
interface UnifiedReportService {
  // Report lifecycle
  createReport(session: SessionContext): Report;
  addFinding(finding: Finding): void;
  addIssue(issue: Issue): void;
  addAction(action: ActionExecution): void;
  completeReport(status: ReportStatus): Report;

  // Queries
  getCurrentReport(): Report | null;
  getReport(id: string): Report | null;

  // User input
  requestUserInput(request: UserInputRequest): void;
  provideUserInput(findingId: string, response: string): void;

  // Events
  onReportChange(callback): Unsubscribe;
}
```

#### 5. ReportPersistenceService (NEW)
Unified persistence:

```typescript
interface ReportPersistenceService {
  // Local persistence
  saveToLocal(report: Report): Promise<void>;
  loadFromLocal(): Promise<Report[]>;

  // Backend sync
  syncToBackend(report: Report): Promise<SyncResult>;
  syncAllPending(): Promise<SyncResult[]>;

  // Configuration
  setBackendConfig(config: BackendConfig): void;
  isBackendAvailable(): Promise<boolean>;
}
```

---

## Improvement Plan - Phases

### Phase 1: Fix Critical Bugs (Immediate)

1. **Actually call backend sync functions**
   - Location: `FindingsTracker.archiveCurrentSession()`
   - Add call to `syncReportToBackend()` when completing
   - Add call to `syncFindingsToBackend()` for session findings

2. **Remove duplicate processing** (if IssueTracker is truly legacy)
   - Option A: Remove IssueTracker entirely
   - Option B: Only call IssueTracker for backward compat marker detection

3. **Add error handling for sync failures**
   - Queue failed syncs for retry
   - Persist to local as fallback

### Phase 2: Refactor for SRP (Short-term)

1. **Extract FindingsParser from FindingsTracker**
   - Move regex patterns to parser
   - Move multi-line buffering to parser
   - Keep tracker as thin orchestration layer

2. **Create SessionManager**
   - Single source of truth for session state
   - All components subscribe to session changes

3. **Split Database Module**
   - `database/connection.rs` - Pool management
   - `database/tasks.rs` - Task run operations
   - `database/sessions.rs` - Session operations
   - `database/settings.rs` - Settings operations
   - `database/migrations.rs` - Migration logic

### Phase 3: Unify Report Model (Medium-term)

1. **Create UnifiedReportService**
   - Aggregate findings + issues + actions
   - Single report lifecycle
   - Consistent event emission

2. **Merge Finding and Issue types**
   - Use categories to distinguish
   - Add `legacy_issue_type` for migration

3. **Create ReportPersistenceService**
   - Abstract local vs backend
   - Handle offline/online sync
   - Queue pending syncs

### Phase 4: Deprecate Legacy (Long-term)

1. **Remove IssueTracker**
   - Update AI prompts to use FINDING markers
   - Remove legacy marker parsing

2. **Remove TestRunReportingService**
   - Already marked deprecated
   - Migrate any remaining usage

3. **Consolidate type definitions**
   - Single `types/reports.ts` for all report types
   - Remove duplicate type files

---

## Specific Refactoring Tasks

### Task 1: Wire Up Backend Sync

**Files:**
- `src/services/FindingsTracker.ts` (line ~700, `archiveCurrentSession`)
- `src/findings/FindingsSync.ts`

**Changes:**
```typescript
// In archiveCurrentSession():
async archiveCurrentSession(status: SessionStatus): Promise<void> {
  const report = this.getCurrentReport();
  if (!report) return;

  const completedReport = this.completeReport(status);

  // ADD: Actually sync to backend
  const projectId = localStorage.getItem("selectedProjectId");
  const backendUrl = localStorage.getItem("backendUrl");
  if (projectId && backendUrl) {
    await syncReportToBackend(completedReport, {
      baseUrl: backendUrl,
      projectId,
    });
    await syncFindingsToBackend(this.getSessionFindings(), {
      baseUrl: backendUrl,
      projectId,
    });
  }

  // Existing archive logic...
}
```

### Task 2: Create SessionManager

**New file:** `src/services/SessionManager.ts`

```typescript
export interface SessionContext {
  id: string;
  actionId: string;
  name: string;
  startedAt: number;
  status: "active" | "completed" | "failed" | "cancelled";
}

export class SessionManager {
  private static instance: SessionManager;
  private currentSession: SessionContext | null = null;
  private listeners = new Set<(session: SessionContext | null) => void>();

  static getInstance(): SessionManager {
    if (!SessionManager.instance) {
      SessionManager.instance = new SessionManager();
    }
    return SessionManager.instance;
  }

  startSession(actionId: string, name = "AI Analysis"): SessionContext {
    // Auto-end previous session if exists
    if (this.currentSession) {
      this.endSession("cancelled");
    }

    this.currentSession = {
      id: crypto.randomUUID(),
      actionId,
      name,
      startedAt: Date.now(),
      status: "active",
    };

    this.notify();
    return this.currentSession;
  }

  endSession(status: SessionContext["status"]): SessionContext | null {
    if (!this.currentSession) return null;

    this.currentSession.status = status;
    const ended = { ...this.currentSession };
    this.currentSession = null;
    this.notify();
    return ended;
  }

  getCurrentSession(): SessionContext | null {
    return this.currentSession;
  }

  subscribe(callback: (session: SessionContext | null) => void): () => void {
    this.listeners.add(callback);
    return () => this.listeners.delete(callback);
  }

  private notify(): void {
    this.listeners.forEach(cb => cb(this.currentSession));
  }
}

export const sessionManager = SessionManager.getInstance();
```

### Task 3: Extract FindingsParser

**New file:** `src/parsers/FindingsParser.ts`

Move all regex patterns and parsing logic from FindingsTracker:
- `FINDING_START_PATTERN`, `FINDING_END_PATTERN`
- `TITLE_PATTERN`, `DESCRIPTION_PATTERN`, etc.
- `parseBlockContent()`
- Multi-line buffer management
- `processSingleLine()` core logic

Keep FindingsTracker as thin orchestration:
- Session/report management
- Event emission
- Coordination with persistence

---

## Files to Modify/Create

### New Files
- `src/services/SessionManager.ts`
- `src/parsers/FindingsParser.ts`
- `src/parsers/IssueParser.ts`
- `src/services/UnifiedReportService.ts`
- `src/services/ReportPersistenceService.ts`
- `src-tauri/src/database/tasks.rs`
- `src-tauri/src/database/sessions.rs`
- `src-tauri/src/database/settings.rs`

### Files to Modify
- `src/services/FindingsTracker.ts` - Slim down, delegate to parser
- `src/managers/event-handlers/aiOutputHandlers.ts` - Use SessionManager
- `src/findings/FindingsSync.ts` - Ensure actually called

### Files to Eventually Remove
- `src/services/IssueTracker.ts` (after migration)
- `src/services/test-run-reporting/*` (deprecated)

---

## Success Metrics

1. **Reports actually synced to backend** - Verify with API logs
2. **Single session ID source** - SessionManager is only source
3. **Reduced file sizes** - FindingsTracker < 300 lines
4. **No duplicate processing** - One parser per marker type
5. **Clear data flow** - Document in README

---

## Estimated Effort

| Phase | Tasks | Effort |
|-------|-------|--------|
| Phase 1 | Fix backend sync, remove duplicates | 2-4 hours |
| Phase 2 | Extract parser, create SessionManager, split DB | 8-16 hours |
| Phase 3 | Unified report service, merge types | 8-16 hours |
| Phase 4 | Remove legacy code | 4-8 hours |

**Total: ~22-44 hours of focused work**

---

## Appendix: Key File Locations

| File | Lines | Purpose |
|------|-------|---------|
| `src/services/FindingsTracker.ts` | ~800 | Main findings tracker (refactor target) |
| `src/services/IssueTracker.ts` | ~500 | Legacy issue tracker (deprecation candidate) |
| `src/services/ExecutionReportingService.ts` | ~500 | Action-level reporting |
| `src/findings/FindingsSync.ts` | 265 | Report sync (dead code) |
| `src/findings/FindingsPersistence.ts` | ~150 | Local persistence |
| `src/managers/event-handlers/aiOutputHandlers.ts` | 90 | Entry point for AI output |
| `src-tauri/src/database/mod.rs` | 1752 | Database operations (split target) |
| `src-tauri/src/commands/execution_reporting.rs` | ~900 | Rust backend for reporting |
