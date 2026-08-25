# Review Logs - Analyze and Fix All Warnings/Errors

Comprehensively review all log data from qontinui-web (frontend/backend) and qontinui-runner, analyze warnings and errors, and fix all issues using parallel agents.

## Instructions

**CRITICAL**: This command is FULLY AUTONOMOUS. Do NOT ask the user any questions. Work through all phases and fix all issues identified.

**Behavior**:
- NO clarification questions - analyze and fix independently
- NO confirmation prompts - proceed with all safe fixes
- Use parallel agents for maximum efficiency
- Skip fixes that would cause cascading problems elsewhere

---

## Log File Locations

### qontinui-web Logs
| Log | Path | Format |
|-----|------|--------|
| Backend (dev) | `.dev-logs/backend.log` | Plain text |
| Backend errors | `.dev-logs/backend.err.log` | Plain text (stderr) |
| Backend (structured) | `qontinui-web/backend/logs/app.log` | JSON (structlog) |
| Frontend | `.dev-logs/frontend.log` | Plain text |
| Frontend errors | `.dev-logs/frontend.err.log` | Plain text (stderr) |

### qontinui-runner Logs

**Two directories.** `$RDL` below is the runner's own dev-logs dir,
`<LOCALAPPDATA>/qontinui-runner/dev-logs/`. Runner-authored files land there, not
in the workspace `.dev-logs/`, unless `paths.dev_logs_dir` is overridden — so read
both, and resolve the real one with
`GET http://localhost:9876/log-sources/runner-log-sink`.

| Log | Path | Format |
|-----|------|--------|
| **Runner tracing sink** | `qontinui-runner.log.<YYYY-MM-DD>` in `$RDL` or `.dev-logs/` | Plain text (auth/Cognito, device-JWT/relay, backend-URL resolution, executor). Daily-rolled, 14-file retention — glob `qontinui-runner.log.*` and take the newest |
| Runner stdout (supervisor capture) | `primary.log` in `$RDL` or `.dev-logs/` (per-runner: `<runner_id>.log`) | Plain text |
| Supervisor tracing | `.dev-logs/supervisor.log` | Plain text |
| Runner errors | `.dev-logs/runner.err.log` | Plain text (stderr/build errors) |
| Python WS debug | `.dev-logs/python-ws-debug.log` | Plain text |

### qontinui-runner Event Logs (JSONL - cleared on startup)
Runner-authored: look in `$RDL` first, `.dev-logs/` second.

| Log | Path | Format |
|-----|------|--------|
| **General events** | `runner-general.jsonl` | JSONL (executor events) |
| **Action logs** | `runner-actions.jsonl` | JSONL (workflow execution tree) |
| **Image recognition** | `runner-image-recognition.jsonl` | JSONL (match details) |
| **Playwright tests** | `runner-playwright.jsonl` | JSONL (test results, specs, console output, page snapshots) |
| **Annotated screenshots** | `screenshots/*.png` | PNG files (visual debug) |
| **Playwright screenshots** | `playwright-screenshots/*.png` | PNG files (test failure screenshots) |
| **AI output** | `ai-output.jsonl` | JSONL (Claude conversations) |
| **Last loaded config** | `.dev-logs/last-loaded-config.*` | JSON/YAML (workflow config) |
| **Config metadata** | `.dev-logs/last-loaded-config.meta.json` | JSON (source path, timestamp) |

**Note**: When started via `dev-start.ps1 -Runner`, the runner automatically captures:
- Vite/React frontend console output (errors, warnings, HMR)
- Rust backend tracing output (with RUST_LOG=info,qontinui_runner=debug)
- Build errors from cargo/rustc
- All executor events to JSONL files including annotated screenshots

---

## Phase 1: Collect All Logs

**CRITICAL: Always read the MOST RECENT logs first.** Old logs may contain errors that have already been fixed. Focus on the last few hundred lines of each log file.

Read all available log files, prioritizing the **end** of each file:

```bash
# Base path
BASE="$PWD"

# IMPORTANT: Use tail to read from the END of files (most recent logs)
# Don't use head or read from beginning - old errors may be stale

# qontinui-web backend logs (most recent 500 lines)
tail -500 "$BASE/.dev-logs/backend.log" 2>/dev/null
tail -200 "$BASE/.dev-logs/backend.err.log" 2>/dev/null
tail -200 "$BASE/qontinui-web/backend/logs/app.log" 2>/dev/null

# qontinui-web frontend logs (most recent 500 lines)
tail -500 "$BASE/.dev-logs/frontend.log" 2>/dev/null
tail -200 "$BASE/.dev-logs/frontend.err.log" 2>/dev/null

# qontinui-runner's own tracing sink — daily-rolled `qontinui-runner.log.<date>`,
# so resolve the NEWEST match, across BOTH dev-logs dirs: the runner usually
# writes to its own app-data dir, not the workspace .dev-logs/.
# Exact dir: GET http://localhost:9876/log-sources/runner-log-sink
# (`runner-tauri.log` is retired as a runner log — it is only stdout capture.)
RDL="$LOCALAPPDATA/qontinui-runner/dev-logs"
RUNNER_LOG=$(ls -t "$BASE"/.dev-logs/qontinui-runner.log.* \
  "$RDL"/qontinui-runner.log.* 2>/dev/null | head -1)
if [ -n "$RUNNER_LOG" ]; then
    tail -500 "$RUNNER_LOG"
else
    echo "NO runner log matched qontinui-runner.log.* in .dev-logs/ or $RDL — runner NOT reviewed"
fi
# Build output (cargo compile) lands in the supervisor's stdout capture
tail -200 "$BASE"/.dev-logs/primary.log "$RDL"/primary.log 2>/dev/null
tail -200 "$BASE/.dev-logs/supervisor.log" 2>/dev/null
tail -200 "$BASE/.dev-logs/runner.err.log" 2>/dev/null
tail -200 "$BASE/.dev-logs/python-ws-debug.log" 2>/dev/null

# qontinui-runner EVENT LOGS (JSONL format - workflow execution details)
# These contain structured workflow execution data and image recognition results.
# Runner-authored, so read both dev-logs dirs.
tail -200 "$BASE"/.dev-logs/runner-general.jsonl "$RDL"/runner-general.jsonl 2>/dev/null
tail -200 "$BASE"/.dev-logs/runner-actions.jsonl "$RDL"/runner-actions.jsonl 2>/dev/null
tail -100 "$BASE"/.dev-logs/runner-image-recognition.jsonl "$RDL"/runner-image-recognition.jsonl 2>/dev/null
tail -100 "$BASE"/.dev-logs/runner-playwright.jsonl "$RDL"/runner-playwright.jsonl 2>/dev/null
tail -100 "$BASE"/.dev-logs/ai-output.jsonl "$RDL"/ai-output.jsonl 2>/dev/null

# List annotated screenshots (these are PNG files for visual debugging)
ls -la "$BASE"/.dev-logs/screenshots/ "$RDL"/screenshots/ 2>/dev/null | tail -20
ls -la "$BASE"/.dev-logs/playwright-screenshots/ "$RDL"/playwright-screenshots/ 2>/dev/null | tail -20

# Last loaded config (use Read tool to view full content)
cat "$BASE/.dev-logs/last-loaded-config.meta.json" 2>/dev/null
ls -la "$BASE/.dev-logs/last-loaded-config."* 2>/dev/null
```

**Why most recent matters:**
- Errors from previous runs may have been fixed
- Build errors (cargo, webpack) at the start of logs are often resolved
- Runtime errors at the END are the current, unfixed issues
- Always check timestamps to ensure errors are from recent execution

---

## Phase 2: Extract Errors and Warnings

**CRITICAL: Search from the END of log files to find the MOST RECENT errors.**

Use `tail` BEFORE `grep` to focus on recent logs:

```bash
BASE="$PWD"

# Backend errors/warnings (from last 1000 lines only)
tail -1000 "$BASE/.dev-logs/backend.log" 2>/dev/null | grep -iE "error|exception|warning|failed|traceback|critical" | tail -100
tail -500 "$BASE/.dev-logs/backend.err.log" 2>/dev/null | grep -iE "error|exception|warning|failed|traceback|critical" | tail -50
tail -500 "$BASE/qontinui-web/backend/logs/app.log" 2>/dev/null | grep -iE '"level":\s*"(error|warning|critical)"' | tail -50

# Frontend errors/warnings (from last 1000 lines only)
tail -1000 "$BASE/.dev-logs/frontend.log" 2>/dev/null | grep -iE "error|warning|failed|unhandled|rejected|exception|\[webpack\]|Type.*not|Cannot find|Module not found" | tail -100
tail -500 "$BASE/.dev-logs/frontend.err.log" 2>/dev/null | grep -iE "error|warning|failed" | tail -50

# qontinui-runner errors (from last 1000 lines only)
# IMPORTANT: the runner's tracing sink is daily-rolled — take the NEWEST match,
# globbing BOTH dev-logs dirs (it usually writes to its own app-data one).
# Build output lands in primary.log, not here.
# Runtime error patterns: [ERROR], [WARNING], [WARN], Traceback, AttributeError
RDL="$LOCALAPPDATA/qontinui-runner/dev-logs"
RUNNER_LOG=$(ls -t "$BASE"/.dev-logs/qontinui-runner.log.* \
  "$RDL"/qontinui-runner.log.* 2>/dev/null | head -1)
if [ -n "$RUNNER_LOG" ]; then
    tail -1000 "$RUNNER_LOG" | grep -iE "\[ERROR\]|\[WARNING\]|\[WARN\]|panic|Traceback|AttributeError|TypeError|Exception" | tail -100
else
    echo "NO runner log matched qontinui-runner.log.* in .dev-logs/ or $RDL — runner errors NOT scanned"
fi
tail -1000 "$BASE"/.dev-logs/primary.log "$RDL"/primary.log 2>/dev/null | grep -iE "\[ERROR\]|\[WARNING\]|\[WARN\]|panic|Traceback|AttributeError|TypeError|Exception" | tail -100
tail -500 "$BASE/.dev-logs/runner.err.log" 2>/dev/null | grep -iE "error|warn|panic|failed" | tail -50

# Python executor logs (these contain actual runtime errors from workflows)
tail -500 "$BASE/.dev-logs/python-ws-debug.log" 2>/dev/null | grep -iE "error|exception|failed|traceback" | tail -50

# qontinui-runner EVENT LOGS (JSONL - search for errors in structured logs)
# Runner-authored, so read both dev-logs dirs.
# General event errors
tail -500 "$BASE"/.dev-logs/runner-general.jsonl "$RDL"/runner-general.jsonl 2>/dev/null | grep -iE '"level":\s*"error"' | tail -50
# Action failures (workflow execution errors)
tail -500 "$BASE"/.dev-logs/runner-actions.jsonl "$RDL"/runner-actions.jsonl 2>/dev/null | grep -iE '"error"|"failed"|"status":\s*"error"' | tail -50
# Image recognition failures (pattern matching issues)
tail -500 "$BASE"/.dev-logs/runner-image-recognition.jsonl "$RDL"/runner-image-recognition.jsonl 2>/dev/null | grep -iE '"found":\s*false' | tail -50
# Playwright test failures
tail -500 "$BASE"/.dev-logs/runner-playwright.jsonl "$RDL"/runner-playwright.jsonl 2>/dev/null | grep -iE '"passed":\s*false|"error"' | tail -50
```

**Pattern explanation for the runner log (`qontinui-runner.log.*`) and `primary.log`:**
- `[ERROR]` / `[WARNING]` - Python executor runtime errors (high priority!)
- `Traceback` - Python stack traces (always investigate!)
- `AttributeError`, `TypeError` - Python runtime errors (code bugs!)
- Ignore: `Compiling`, `Building`, `Downloading` - these are just build output

---

## Phase 3: Categorize Issues

After collecting errors/warnings, categorize them:

### Category 1: Backend Errors (Python/FastAPI)
- **Import errors** - Missing dependencies, circular imports
- **Type errors** - mypy/runtime type issues
- **Database errors** - SQLAlchemy, migration issues
- **API errors** - Endpoint failures, validation errors
- **Authentication errors** - JWT, session issues

### Category 2: Frontend Errors (Next.js/React)
- **Build errors** - Webpack, TypeScript compilation
- **Runtime errors** - React component errors, hydration mismatches
- **API call failures** - Network errors, CORS issues
- **Type errors** - TypeScript strict mode violations
- **Deprecation warnings** - React, Next.js deprecated APIs

### Category 3: qontinui-runner Errors (Tauri/Rust)
- **Rust errors** - Compilation, runtime panics
- **IPC errors** - Tauri command failures
- **Python subprocess errors** - Qontinui execution issues
- **WebSocket errors** - Connection, message handling

---

## Phase 4: Spawn Parallel Agents for Fixes

**CRITICAL**: Use the Task tool to spawn multiple agents in parallel. Each agent handles one category.

### Agent 1: Backend Fixer
```
Task the agent to:
1. Read the backend errors extracted in Phase 2
2. For each error:
   - Identify the source file and line number
   - Read the relevant code
   - Determine the root cause
   - Implement a fix (if safe)
3. Run `poetry run mypy --package app` to verify fixes
4. Report: errors found, errors fixed, errors skipped (with reason)
```

### Agent 2: Frontend Fixer
```
Task the agent to:
1. Read the frontend errors extracted in Phase 2
2. For each error:
   - Identify the source file from the stack trace
   - Read the relevant React/Next.js code
   - Determine the root cause
   - Implement a fix (if safe)
3. Run `npm run typecheck && npm run lint` to verify
4. Report: errors found, errors fixed, errors skipped (with reason)
```

### Agent 3: Runner Fixer
```
Task the agent to:
1. Read the runner errors extracted in Phase 2
2. For each error:
   - Identify if it's Rust, TypeScript, or Python related
   - Read the relevant code in qontinui-runner
   - Determine the root cause
   - Implement a fix (if safe)
3. Run `cargo check` (Rust) and `npm run typecheck` (TS) to verify
4. Report: errors found, errors fixed, errors skipped (with reason)
```

---

## Phase 5: Agent Task Template

For each agent, use this Task tool prompt template:

```
You are fixing {SERVICE} errors/warnings found in the logs.

## Errors to Fix
{PASTE_ERRORS_HERE}

## Instructions
1. For each error, identify the source file and line
2. Read the file to understand context
3. Determine if the fix is safe (won't break other functionality)
4. If safe, implement the fix
5. If unsafe or unclear, add to "skipped" list with reason

## Fix Guidelines
- **Import errors**: Add missing imports, fix import paths
- **Type errors**: Add proper type annotations, fix mismatches
- **Null/undefined errors**: Add proper null checks
- **Deprecation warnings**: Update to new API
- **Runtime errors**: Fix the logic error, add error handling

## DO NOT Fix
- Errors that are symptoms of missing database data
- Errors that require API/schema changes
- Errors in third-party libraries
- Errors that would require significant refactoring

## Verification
After fixes, run:
- Python: `poetry run black . && poetry run isort . && poetry run mypy --package {PKG}`
- TypeScript: `npm run typecheck && npm run lint`
- Rust: `cargo check && cargo clippy`

## Report Format
Return a structured report:
- Total errors found: X
- Errors fixed: Y
- Errors skipped: Z
- List of fixes made (file:line - description)
- List of skipped errors (with reason)
```

---

## Phase 6: Restart Services (If Needed)

After fixes are applied, restart affected services:

```bash
# If backend code changed
$PWD/qontinui-claude-config/scripts/restart-services.sh backend

# If frontend code changed
$PWD/qontinui-claude-config/scripts/restart-services.sh frontend clean

# If runner code changed (user must restart manually or use):
powershell.exe -Command "Stop-Process -Name qontinui-runner -Force -ErrorAction SilentlyContinue; Start-Sleep -Seconds 2; cd '$PWD\qontinui-runner'; Start-Process -FilePath 'npm.cmd' -ArgumentList 'run','tauri','dev' -WindowStyle Normal"
```

---

## Phase 7: Verify Logs Are Clean

After services restart, check logs again:

```bash
BASE="$PWD"

# Wait for services to stabilize
sleep 15

# Check for new errors (should be significantly reduced)
echo "=== Backend Errors ==="
grep -iE "error|exception|critical" "$BASE/.dev-logs/backend.log" 2>/dev/null | tail -20

echo "=== Frontend Errors ==="
grep -iE "error|exception|failed" "$BASE/.dev-logs/frontend.log" 2>/dev/null | tail -20

echo "=== Runner Errors ==="
RDL="$LOCALAPPDATA/qontinui-runner/dev-logs"
RUNNER_LOG=$(ls -t "$BASE"/.dev-logs/qontinui-runner.log.* \
  "$RDL"/qontinui-runner.log.* 2>/dev/null | head -1)
if [ -n "$RUNNER_LOG" ]; then
    grep -iE "\[ERROR\]|\[WARNING\]|error|panic|Traceback" "$RUNNER_LOG" | tail -20
else
    echo "NO runner log matched qontinui-runner.log.* in .dev-logs/ or $RDL — cannot confirm runner is clean"
fi
```

---

## Phase 8: Summary Report

Generate a comprehensive report:

```markdown
# Log Review Complete

## Logs Analyzed
- Backend log: X lines, Y errors, Z warnings
- Frontend log: X lines, Y errors, Z warnings
- Runner log: X lines, Y errors, Z warnings

## Fixes Applied

### Backend ({count} fixes)
- file.py:123 - Fixed import error for module X
- file.py:456 - Added type annotation for function Y

### Frontend ({count} fixes)
- Component.tsx:45 - Fixed null reference in useEffect
- page.tsx:89 - Updated deprecated Next.js API

### Runner ({count} fixes)
- main.rs:234 - Fixed unwrap panic, added proper error handling

## Skipped Issues
- {list issues that were intentionally skipped with reasons}

## Remaining Warnings
- {list any warnings that persist but are not errors}

## Services Restarted
- [x] Backend
- [x] Frontend
- [ ] Runner (manual restart required / not running)

## Verification
- Backend logs clean: Yes/No
- Frontend logs clean: Yes/No
- Runner logs clean: Yes/No
```

---

## Error Type Reference

### Common Backend Errors
| Error Pattern | Likely Cause | Fix |
|---------------|--------------|-----|
| `ImportError: cannot import` | Circular import or missing dep | Fix import order, add dep |
| `TypeError: expected str` | Type mismatch | Add type conversion |
| `ValidationError` | Pydantic validation failed | Fix model or input data |
| `sqlalchemy.exc.*` | Database issue | Fix query or migration |
| `KeyError` | Missing dict key | Add `.get()` or check |

### Common Frontend Errors
| Error Pattern | Likely Cause | Fix |
|---------------|--------------|-----|
| `TypeError: Cannot read properties of undefined` | Null reference | Add optional chaining |
| `Hydration failed` | SSR/client mismatch | Fix component rendering |
| `Module not found` | Missing import | Fix import path |
| `Type 'X' is not assignable` | TypeScript type error | Fix types |
| `Warning: React.* is deprecated` | Old API | Update to new API |

### Common Runner Errors
| Error Pattern | Likely Cause | Fix |
|---------------|--------------|-----|
| `thread 'main' panicked` | Rust panic | Add proper error handling |
| `called unwrap() on None` | Option unwrap | Use `ok_or()` or match |
| `IPC error` | Tauri command failed | Fix command implementation |
| `WebSocket connection failed` | Connection issue | Add retry logic |

---

## Safety Rules

**DO Fix:**
- Clear type errors with obvious solutions
- Missing null checks that cause crashes
- Deprecated API usage with documented migration paths
- Import errors from typos or wrong paths
- Simple logic errors with clear fixes

**DO NOT Fix:**
- Errors that might be intentional (e.g., error logging)
- Issues requiring database schema changes
- Problems in generated or third-party code
- Errors that would require significant refactoring
- Issues where the fix is ambiguous

**When In Doubt:**
- Add a `# TODO: Review - {reason}` comment
- Log the issue in the "Skipped" section
- Move on to the next error

---

## CRITICAL: Thorough Error Investigation Required

**NEVER skip an error without thorough investigation.** Many errors that appear to be "data issues" or "external problems" are actually code bugs that should be fixed.

### Before Skipping ANY Error, You MUST:

1. **Read the stack trace completely** - Identify the exact file and line number
2. **Read the source code** - Understand what the code is trying to do
3. **Trace the data flow** - Where does the problematic data come from?
4. **Ask: "Is there missing error handling?"** - Should the code handle this case gracefully?
5. **Ask: "Is there a stale reference?"** - Could localStorage, state, or cache have outdated data?
6. **Ask: "Should the code recover from this?"** - Even if data is bad, should the code fail gracefully?

### Common Errors That LOOK Like Data Issues But Are Code Bugs:

| Error Pattern | Wrong Assumption | Actual Fix |
|---------------|------------------|------------|
| `404 PROJECT_NOT_FOUND` | "User deleted the project" | Code should clear stale project ID from localStorage |
| `401 Unauthorized` | "User's token expired" | Code should refresh token or redirect to login |
| `Cannot read property of undefined` | "API returned bad data" | Code should add null checks or validate response |
| `Failed to fetch` | "Network issue" | Code should add retry logic or error boundary |
| `Entity not found` | "Database is missing data" | Code should handle missing entities gracefully |

### Investigation Template

For EACH error before deciding to skip, document:

```markdown
### Error: [Error message]
**Stack trace location:** [file:line]
**Code examined:** [Yes/No]
**Root cause:** [What's actually happening]
**Why it's happening:** [User action, stale data, missing handling, etc.]
**Can code fix this?** [Yes - describe fix / No - explain why]
**Decision:** [FIX / SKIP with specific reason]
```

### Examples of WRONG Skip Reasons:

❌ "This is a data issue" - WHY is there bad data? Can code prevent/handle it?
❌ "User probably deleted the project" - Code should handle deleted projects gracefully
❌ "External service returned error" - Code should handle external failures
❌ "This only happens sometimes" - Intermittent bugs are still bugs
❌ "Looks like a race condition" - Race conditions must be fixed

### Examples of VALID Skip Reasons:

✅ "Third-party library internal error, no way to intercept" - with link to upstream issue
✅ "Error is intentionally logged for debugging, not a failure"
✅ "Requires API schema change that would break other clients"
✅ "Fix would require major refactoring of unrelated code" - create TODO issue instead

### The Rule

**If you're about to skip an error, stop and ask: "If this error appeared in the console while the user was demoing the product, would they be embarrassed?"**

If YES → Find a way to fix it, even if it's just better error handling
If NO → Document why it's acceptable and skip

---

## Execution Mode

This command should execute in the following order:
1. **Sequential**: Phase 1-3 (collect and categorize)
2. **Parallel**: Phase 4-5 (spawn 3 agents simultaneously)
3. **Sequential**: Phase 6-8 (restart, verify, report)

Total expected time: 5-15 minutes depending on error count.
