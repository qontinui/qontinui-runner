# Find Misplaced Functionality

Search all Qontinui repositories for code that doesn't fit its repo's purpose, identify the correct location, and migrate it.

## How Continuation Works

**The runner handles continuation automatically!**

1. You work on analysis and migrations
2. Output `[TASK_COMPLETE]` when done
3. If your session ends early, the runner continues with your previous output as context

Just document your progress - it becomes context for continuation.

---

## Repository Descriptions

Use these descriptions to determine if functionality is correctly placed:

### Core Libraries

**qontinui**
- **Purpose**: Core Python library for visual automation (uses multistate for state management)
- **Should contain**:
  - HAL (Hardware Abstraction Layer): Screen capture, input control
  - Vision module: Pattern matching, object detection, OCR algorithms
  - Action system and composable automation actions (Click, Type, Find, Wait)
  - Workflow engine for task orchestration
  - Qontinui-specific state management (wrappers around multistate for CV/automation)
- **Should NOT contain**:
  - Web/API endpoints (→ qontinui-web)
  - Desktop UI code (→ qontinui-runner)
  - Training/fine-tuning logic (→ qontinui-train or qontinui-finetune)
  - MCP server implementations (→ qontinui-mcp)
  - Development tools (→ qontinui-devtools)
  - Generic state machine logic (→ multistate)
  - Pydantic schemas only (→ qontinui-schemas)
- **Technologies**: Python, PyTorch, OpenCV, Transformers, multistate

**multistate**
- **Purpose**: Standalone generic state machine library (NOT Qontinui-specific)
- **Should contain**:
  - Generic multi-state state machine implementation
  - State transition logic and pathfinding
  - State groups and hidden states
  - Event handling and state coordination
  - Testing/exploration utilities for state machines
  - Documentation site (Docusaurus)
- **Should NOT contain**:
  - Qontinui-specific automation logic (→ qontinui)
  - Vision/CV code (→ qontinui)
  - Web services (→ qontinui-web)
  - Desktop UI (→ qontinui-runner)
  - Heavy ML/CV dependencies (keep lightweight)
- **Key distinction**: This is a general-purpose library that could be used by ANY project needing multi-state management, not just Qontinui
- **Technologies**: Python (3.10+), typing-extensions, dataclasses-json

**qontinui-schemas**
- **Purpose**: Lightweight shared Pydantic schemas for the Qontinui ecosystem
- **Should contain**:
  - Pydantic models for workflow configuration
  - Action schemas (ClickConfig, FindConfig, TypeConfig, etc.)
  - Property groups (CoreProperties, VisionProperties, TimingProperties)
  - RAG models (SearchResult, DocumentChunk)
  - Data contracts shared across all Qontinui services
- **Should NOT contain**:
  - Heavy dependencies (ML/CV libraries) - keep MINIMAL
  - Business logic or implementation code (→ qontinui)
  - API endpoints (→ qontinui-web)
  - Automation execution code (→ qontinui)
- **Why it exists**: Main `qontinui` package has heavy dependencies (PyTorch, OpenCV), but web services just need schema definitions.
- **Technologies**: Python, Pydantic (only dependency)

**qontinui-web**
- **Purpose**: Web application for managing automations
- **Should contain**:
  - **Backend**: Authentication, user management, project CRUD, billing, analytics
  - **Frontend**: UI for creating/managing workflows, project management, settings
- **Should NOT contain**:
  - Core automation logic (→ qontinui)
  - Desktop runner logic (→ qontinui-runner)
  - Training pipelines (→ qontinui-train)
  - Development tools (→ qontinui-devtools)
- **Technologies**: Next.js 15, React 19, FastAPI, PostgreSQL, Redis

### Desktop Application

**qontinui-runner**
- **Purpose**: Desktop application for executing automations
- **Should contain**:
  - Tauri IPC layer
  - React UI for desktop runner
  - Rust backend for runner orchestration
  - Python subprocess management for qontinui execution
  - Local workflow execution
- **Should NOT contain**:
  - Core automation logic (→ qontinui)
  - Web endpoints (→ qontinui-web)
  - Training code (→ qontinui-train)
- **Technologies**: Tauri (Rust + TypeScript), React

### Machine Learning

**qontinui-train**
- **Purpose**: ML training pipelines
- **Should contain**:
  - Training scripts
  - Dataset preparation
  - Model evaluation
  - Hyperparameter tuning
  - Training orchestration
- **Should NOT contain**:
  - Inference code (→ qontinui)
  - Fine-tuning logic (→ qontinui-finetune)
  - Development tools (→ qontinui-devtools)
- **Technologies**: Python, PyTorch, ML frameworks

**qontinui-finetune**
- **Purpose**: Model fine-tuning tools
- **Should contain**:
  - Fine-tuning scripts for pretrained models
  - Adapter training (LoRA, etc.)
  - Domain adaptation
  - Few-shot learning pipelines
- **Should NOT contain**:
  - Full training from scratch (→ qontinui-train)
  - Inference code (→ qontinui)
  - Development tools (→ qontinui-devtools)
- **Technologies**: Python, PyTorch, Transformers

### Integration & Tools

**qontinui-mcp**
- **Purpose**: MCP server for Claude to execute automations
- **Should contain**:
  - MCP protocol implementation
  - Tool definitions for Claude
  - HTTP client for qontinui-runner API
  - Simple status checking
- **Should NOT contain**:
  - Core automation logic (→ qontinui)
  - Runner implementation (→ qontinui-runner)
  - Web services (→ qontinui-web)
  - Database management
- **Technologies**: Python, MCP protocol

**qontinui-devtools**
- **Purpose**: Development utilities
- **Should contain**:
  - Code analysis tools (god classes, circular deps, dead code)
  - Linting runners
  - Type checking utilities
  - Security scanners
  - Dependency analyzers
- **Should NOT contain**:
  - Core automation logic (→ qontinui)
  - Training code (→ qontinui-train)
- **Technologies**: Python, static analysis tools

**qontinui-claude-config**
- **Purpose**: Claude Code configuration and slash commands
- **Should contain**:
  - .claude/ directory with commands
  - Development scripts (dev-start.ps1, restart-services.sh)
  - CLAUDE.md instructions
  - MCP configuration
- **Should NOT contain**:
  - Core automation logic (→ qontinui)
  - Production code (any other repo)
  - Training/fine-tuning code
- **Technologies**: Markdown, PowerShell, Bash

---

## Instructions

### Step 1: Launch Parallel Analysis Agents

Launch one agent per repository (11 total) using the Task tool.

**Agent Prompt Template**:

```
Analyze the {REPO_NAME} repository for misplaced functionality.

**Repository Purpose**: {PURPOSE_DESCRIPTION}
**Should contain**: {SHOULD_CONTAIN_LIST}
**Should NOT contain**: {SHOULD_NOT_CONTAIN_LIST}

**Analysis Steps**:
1. Use Glob to find all source files (*.py, *.ts, *.tsx, *.rs)
2. Use Grep to search for patterns suggesting misplaced code
3. Read suspicious files to verify misplacement
4. Report findings

For each issue found, report:
- Priority: critical|high|medium|low
- Location: file_path:line_range
- Lines of code: count
- Why misplaced: explanation
- Destination: target_repo/suggested_path
- Complexity: LOW|MEDIUM|HIGH

If no issues found, report "No misplaced functionality detected".
```

**Launch all 11 agents in a SINGLE message with multiple Task tool calls.**

### Step 2: Consolidate Results

After all agents complete:

1. Combine all findings
2. Group by destination repository
3. Sort by priority (critical → low)
4. Calculate total migration effort

### Step 3: User Approval

Ask user which migrations to proceed with:
- All critical issues?
- Include high priority?
- Include medium/low?
- Specific items only?

### Step 4: Execute Migrations

For each approved migration:

1. Move the code to destination
2. Update imports in source and destination
3. Run tests to verify
4. Commit the change

Document each migration as you complete it (this becomes context for continuation).

### Step 5: Summary

After all migrations, output:

```
Migration complete. {N} issues resolved.

## Summary
- Migrated X files from A to B
- Updated Y import statements
- All tests passing

[TASK_COMPLETE]
```

---

## Search Patterns for Common Misplacements

### Misplaced in qontinui (core library)

```bash
# Web frameworks in core
grep -r "@app.route\|@router.get\|@router.post" qontinui/src/

# UI frameworks in core
grep -r "import.*react\|from 'react'" qontinui/src/

# Training code in core
grep -r "torch.optim\|DataLoader\|train_epoch" qontinui/src/
```

### Misplaced in qontinui-web

```bash
# CV code in web
grep -r "cv2\.\|torch\.\|transformers\." qontinui-web/backend/app/ qontinui-web/frontend/src/

# Training in web
grep -r "train_model\|optimizer\.step" qontinui-web/
```

### Misplaced in qontinui-runner

```bash
# Core logic in runner
grep -r "class.*Engine\|class.*Workflow" qontinui-runner/src-tauri/src/

# Web endpoints in runner
grep -r "@app.route\|FastAPI\|Express" qontinui-runner/
```

---

## Priority Guidelines

**Critical (Fix immediately)**:
- Circular dependencies between repos
- Duplicate functionality causing inconsistency
- Core logic in wrong repo causing coupling

**High (Fix soon)**:
- Clear architectural violations
- Code that blocks proper separation of concerns
- Public APIs in wrong package

**Medium (Fix when convenient)**:
- Utilities in suboptimal location but working
- Code that could be better organized

**Low (Nice to have)**:
- Minor organizational improvements
- Code that's technically correct but could be clearer

---

## Notes

- **Be conservative**: Only flag items with clear misplacement
- **Check git history**: Some "misplaced" code may have historical reasons
- **Verify tests**: Always run tests before and after migration
- **Update docs**: Keep documentation in sync with code locations
- **Consider dependencies**: Some migrations may require publishing new package versions

## Arguments

$ARGUMENTS
