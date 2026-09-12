# Analyze Automation Results

Review automation results, screenshots, and logs. Analyze for issues and fix them using parallel agents. Re-run the loaded automation to verify fixes.

**This command is triggered automatically by Qontinui automation workflows.**

## Instructions

**CRITICAL**: This command is FULLY AUTONOMOUS. Do NOT ask the user any questions. Analyze all data, fix all issues, and re-run automation.

---

## Phase 1: Review Collected Data

### Step 1.1: Read Execution Results
```bash
BASE="$PWD"

# Read main execution results
cat "$BASE/.automation-results/latest/execution.json" 2>/dev/null

# List all files in results directory
ls -la "$BASE/.automation-results/latest/" 2>/dev/null

# Check for screenshots
ls "$BASE/.automation-results/latest/screenshots/" 2>/dev/null
ls "$BASE/.automation-results/latest/" 2>/dev/null | grep -E "\.(png|jpg|jpeg)$"
```

### Step 1.2: View Screenshots
Use the Read tool to view any screenshots found:
- Failure screenshots (captured at error points)
- Checkpoint screenshots (captured at verification points)
- Step screenshots (captured during workflow execution)

**IMPORTANT**: Always view screenshots - they contain critical visual information about UI state, errors, and unexpected behavior.

### Step 1.3: Read Captured Log Snapshots
```bash
BASE="$PWD"

# Check for log snapshots captured during automation
ls "$BASE/.automation-results/latest/logs/" 2>/dev/null

# Read captured logs
cat "$BASE/.automation-results/latest/logs/backend.log" 2>/dev/null | tail -200
cat "$BASE/.automation-results/latest/logs/frontend.log" 2>/dev/null | tail -200
# The runner's own tracing sink is daily-rolled (`qontinui-runner.log.<date>`),
# so snapshots carry a dated name — read the newest one.
SNAP_RUNNER_LOG=$(ls -t "$BASE"/.automation-results/latest/logs/qontinui-runner.log.* 2>/dev/null | head -1)
if [ -n "$SNAP_RUNNER_LOG" ]; then
    tail -200 "$SNAP_RUNNER_LOG"
else
    echo "NO snapshot runner log matched .automation-results/latest/logs/qontinui-runner.log.*"
fi
```

### Step 1.4: Read Runner Event Logs (CRITICAL for workflow debugging)
```bash
BASE="$PWD"

# Runner event logs (JSONL format - structured workflow execution data)
# These contain detailed workflow execution information.
# The runner writes them next to its own sink, which is usually its app-data
# dev-logs dir, NOT the workspace .dev-logs/ — so read both locations.
# Exact dir: GET http://localhost:9876/log-sources/runner-log-sink
RDL="$LOCALAPPDATA/qontinui-runner/dev-logs"
tail -100 "$BASE"/.dev-logs/runner-general.jsonl "$RDL"/runner-general.jsonl 2>/dev/null
tail -100 "$BASE"/.dev-logs/runner-actions.jsonl "$RDL"/runner-actions.jsonl 2>/dev/null

# Image recognition logs (CRITICAL - shows all pattern match attempts)
tail -50 "$BASE"/.dev-logs/runner-image-recognition.jsonl "$RDL"/runner-image-recognition.jsonl 2>/dev/null

# Playwright test logs (test results, specs, console output, page snapshots)
tail -50 "$BASE"/.dev-logs/runner-playwright.jsonl "$RDL"/runner-playwright.jsonl 2>/dev/null

# AI output logs (Claude conversations during automation)
tail -100 "$BASE"/.dev-logs/ai-output.jsonl "$RDL"/ai-output.jsonl 2>/dev/null

# Annotated screenshots from image recognition (use Read tool to view these!)
ls -la "$BASE"/.dev-logs/screenshots/ "$RDL"/screenshots/ 2>/dev/null | tail -20

# Playwright test failure screenshots
ls -la "$BASE"/.dev-logs/playwright-screenshots/ "$RDL"/playwright-screenshots/ 2>/dev/null | tail -20

# Last loaded config (use Read tool to view full content)
cat "$BASE/.dev-logs/last-loaded-config.meta.json" 2>/dev/null
ls -la "$BASE/.dev-logs/last-loaded-config."* 2>/dev/null
```

**IMPORTANT**: Use the Read tool to view the annotated screenshots the listing above found (they are usually under the runner's own dev-logs dir, not the workspace `.dev-logs/`). These show:
- Match locations with colored boxes
- Confidence scores
- Why patterns failed to match

---

## Phase 2: Analyze for Issues

### Step 2.1: Parse Execution Results
From `execution.json`, extract:
- `success` - Overall success/failure
- `failed_steps` - List of steps that failed
- `errors` - Error messages
- `console_errors` - Browser/React console errors
- `network_failures` - Failed API calls
- `assertions` - Failed assertions/expectations
- `timing` - Steps that took too long

### Step 2.2: Analyze Screenshots
For each screenshot:
1. View the image using the Read tool
2. Look for:
   - Error dialogs or modals
   - Missing UI elements
   - Incorrect UI state
   - Layout/styling issues
   - Loading spinners stuck
   - Empty states where data should exist

### Step 2.3: Analyze Logs
Search for errors in the captured logs:
```bash
BASE="$PWD"

# Search for errors in captured logs
grep -iE "error|exception|failed|panic|traceback" "$BASE/.automation-results/latest/logs/"* 2>/dev/null | head -100

# If no captured logs, check live logs
grep -iE "error|exception|failed|panic" "$BASE/.dev-logs/backend.log" 2>/dev/null | tail -50
grep -iE "error|exception|failed" "$BASE/.dev-logs/frontend.log" 2>/dev/null | tail -50
# Runner's own tracing sink — daily-rolled, so resolve the newest file, and look
# in the runner's app-data dev-logs dir as well as the workspace one.
# (`runner-tauri.log` is retired as a runner log — it is only stdout capture.)
RDL="$LOCALAPPDATA/qontinui-runner/dev-logs"
RUNNER_LOG=$(ls -t "$BASE"/.dev-logs/qontinui-runner.log.* \
  "$RDL"/qontinui-runner.log.* 2>/dev/null | head -1)
if [ -n "$RUNNER_LOG" ]; then
    grep -iE "error|warn|panic" "$RUNNER_LOG" | tail -50
else
    echo "NO runner log matched qontinui-runner.log.* in .dev-logs/ or $RDL — runner errors NOT checked"
fi

# Check runner event logs for workflow execution issues (both dev-logs dirs)
grep -iE '"level":\s*"error"' "$BASE"/.dev-logs/runner-general.jsonl "$RDL"/runner-general.jsonl 2>/dev/null | tail -50
grep -iE '"error"|"failed"' "$BASE"/.dev-logs/runner-actions.jsonl "$RDL"/runner-actions.jsonl 2>/dev/null | tail -50
# Find failed image recognitions (pattern not found)
grep -iE '"found":\s*false' "$BASE"/.dev-logs/runner-image-recognition.jsonl "$RDL"/runner-image-recognition.jsonl 2>/dev/null | tail -50
# Find failed Playwright tests
grep -iE '"passed":\s*false|"error"' "$BASE"/.dev-logs/runner-playwright.jsonl "$RDL"/runner-playwright.jsonl 2>/dev/null | tail -50
```

### Step 2.4: View Annotated Screenshots
**CRITICAL**: For any image recognition failures, use the Read tool to view the annotated screenshots:
```bash
# List recent annotated screenshots (both dev-logs dirs)
ls -la "$BASE"/.dev-logs/screenshots/ "$LOCALAPPDATA"/qontinui-runner/dev-logs/screenshots/ 2>/dev/null | tail -10
```
Then use the Read tool on the PNG files to see visual debug info showing where the pattern was searched and why it failed.

### Step 2.5: Categorize Issues
Create a list of all issues found, categorized by:

| Category | Source | Examples |
|----------|--------|----------|
| **Backend** | execution.json, logs | API 500 errors, database errors, auth failures |
| **Frontend** | screenshots, console_errors | React errors, missing elements, UI bugs |
| **Runner** | logs, execution.json | Rust panics, IPC failures, Python errors |
| **Automation** | execution.json | Selector not found, timeout, assertion failed |

---

## Phase 3: Fix Issues with Parallel Agents

**CRITICAL**: Spawn one agent per issue for maximum parallelism.

### Agent Task Template

For each issue identified, spawn a Task agent with this prompt:

```
You are fixing a specific issue found during Qontinui automation testing.

## Issue Details
- **Category**: {CATEGORY}
- **Error**: {ERROR_MESSAGE}
- **Source**: {WHERE_ERROR_WAS_FOUND}
- **Screenshot**: {SCREENSHOT_PATH_IF_AVAILABLE}
- **Stack trace**: {STACK_TRACE_IF_AVAILABLE}

## Instructions
1. Read the relevant source code based on error location
2. Understand the root cause of the issue
3. Implement a fix
4. Verify the fix compiles/passes linting

## Fix Guidelines by Category

### Backend Issues (qontinui-web/backend)
- Check API endpoint handlers in `app/api/`
- Check models and schemas in `app/models/` and `app/schemas/`
- Check services in `app/services/`
- Run: `cd qontinui-web/backend && poetry run mypy --package app`

### Frontend Issues (qontinui-web/frontend)
- Check React components in `src/components/`
- Check pages in `src/app/`
- Check API calls in `src/lib/api/`
- Run: `cd qontinui-web/frontend && npm run typecheck`

### Runner Issues (qontinui-runner)
- Check Rust code in `src-tauri/src/`
- Check TypeScript in `src/`
- Run: `cd qontinui-runner && cargo check` and `npm run typecheck`

### Automation Issues
- These may not require code fixes
- Could be timing issues (add waits)
- Could be selector issues (update selectors)
- Could be test data issues

## Report
Return:
- What the issue was
- Root cause identified
- Fix implemented (file:line)
- Verification passed (yes/no)
```

### Spawn Agents in Parallel

Use the Task tool to spawn multiple agents simultaneously:

```
Task 1: Fix backend issue X
Task 2: Fix frontend issue Y
Task 3: Fix runner issue Z
...
```

---

## Phase 4: Restart Services (Only If Code Changed)

**IMPORTANT**: Only restart services if code was actually changed in Phase 3.

- If **NO issues were found** or issues were **automation-only** (timing, selectors), skip this phase entirely.
- If issues were found but **no code changes were made**, skip this phase.
- Only restart the specific service whose code was modified.

**NEVER restart the qontinui-runner unless you specifically modified files in the qontinui-runner directory.**

```bash
# ONLY if backend code (qontinui-web/backend) was changed:
$PWD/qontinui-claude-config/scripts/restart-services.sh backend

# ONLY if frontend code (qontinui-web/frontend) was changed:
$PWD/qontinui-claude-config/scripts/restart-services.sh frontend clean

# ONLY if qontinui-runner Rust code (src-tauri/) was changed - requires full restart:
powershell.exe -Command "Stop-Process -Name qontinui-runner -Force -ErrorAction SilentlyContinue; Start-Sleep -Seconds 2; cd '$PWD\qontinui-runner'; Start-Process -FilePath 'npm.cmd' -ArgumentList 'run','tauri','dev' -WindowStyle Normal"
# IMPORTANT: After runner restart, restore config/workflow/monitor by calling:
python $PWD/qontinui-claude-config/scripts/qontinui-http.py load-last-config
```

**Note on qontinui-runner hot-reload:**
- **Frontend (React/TypeScript)**: Hot-reloads automatically via Vite HMR - NO restart needed
- **Rust backend**: Tauri auto-rebuilds when Rust code changes - the app will restart automatically

Wait for restarted services to be ready (only if services were restarted):
```bash
sleep 15
```

---

## Phase 5: Re-run the Automation

**CRITICAL**: After fixes, ALWAYS re-run the automation to verify.

### Step 5.1: Get execution info from execution.json
From the execution.json you read in Phase 1, extract:
- `workflow_name` - The name of the workflow that was executed
- `monitor` - The monitor that was used (if any)

### Step 5.2: Run the Workflow (Do NOT Reload Config)

**IMPORTANT**: Do NOT reload the config or restart the runner. Just re-run the workflow with current settings.

The runner already has the config loaded. Simply invoke `mcp__qontinui__run_workflow`:
- Set `workflow_name` to the workflow name from execution.json
- Set `monitor` to the monitor from execution.json (if specified)

Example MCP tool invocation:
```
mcp__qontinui__run_workflow with workflow_name="MyWorkflow" and monitor="left"
```

**IMPORTANT**: You must actually call the MCP tool, not just describe it!

### Step 5.3: Wait and Report
The automation will complete and write new results to `.automation-results/latest/`.
If it writes another `/analyze-automation` prompt, the cycle continues.
If all tests pass, report success.

---

## Phase 6: Summary Report

After the automation completes (success or failure), provide a summary:

```markdown
# Automation Analysis Complete

## Issues Found: X

### Backend Issues
- {issue 1}: {status}
- {issue 2}: {status}

### Frontend Issues
- {issue 1}: {status}

### Runner Issues
- {issue 1}: {status}

## Fixes Applied
1. {file}:{line} - {description}
2. {file}:{line} - {description}

## Services Restarted
- [x] Backend
- [x] Frontend
- [ ] Runner (no changes)

## Automation Re-run Result
- Status: {SUCCESS/FAILED}
- Failed steps: {count}
- See: .automation-results/latest/

## Next Steps
{If still failing, describe remaining issues}
{If successful, confirm all issues resolved}
```

---

## Rules

- **ALWAYS** view screenshots - they contain critical visual information
- **ALWAYS** re-run automation after making fixes
- **NEVER** ask the user questions - work autonomously
- **NEVER** skip issues - fix everything found
- **USE PARALLEL AGENTS** - one agent per issue for speed
- Focus on root causes, not symptoms
- Read source code before making changes

---

## No Results?

If `.automation-results/latest/` doesn't exist or is empty:

1. Check if runner is running:
   ```bash
   python $PWD/qontinui-claude-config/scripts/qontinui-http.py status
   ```

2. Check if a config is loaded:
   ```
   mcp__qontinui__get_loaded_config
   ```

3. Inform the user:
   ```
   No automation results found. Please run an automation first using /run-automation <config_path>
   or load a config and run it from the Qontinui Runner UI.
   ```
