# QA Automation Review

Analyze the latest automation results and fix any issues found. After making fixes, re-run the automation to verify. This creates a feedback loop until all tests pass.

## Instructions

### Step 1: Read Automation Results
```bash
cat $PWD/.automation-results/latest/execution.json
```

If no results exist, inform the user they need to run automation first:
```
/run-automation <config_path>
```

### Step 2: Read Log Snapshots (if available)
The automation captures log snapshots at execution time:
```bash
# Check if log snapshots exist
ls $PWD/.automation-results/latest/logs/

# Read relevant logs for failed tests
cat $PWD/.automation-results/latest/logs/backend.log
cat $PWD/.automation-results/latest/logs/frontend.log
# The runner's tracing sink is daily-rolled, so the snapshot is dated. Guard the
# glob: unguarded it prints a literal-glob "No such file" when the snapshot has
# no runner log, which reads like an empty log rather than a missing one.
SNAP_LOG=$(ls -t "$PWD"/.automation-results/latest/logs/qontinui-runner.log.* 2>/dev/null | head -1)
if [ -n "$SNAP_LOG" ]; then
    cat "$SNAP_LOG"
else
    echo "NO runner log in this snapshot (.automation-results/latest/logs/qontinui-runner.log.*) — runner NOT checked"
fi
```

### Step 3: Analyze Failures
For each failed test in the results:
1. Read the error message and failed step details
2. Check `console_errors` for JavaScript errors
3. Check `network_failures` for API issues
4. Check the log snapshots for server-side errors
5. View failure screenshot if path is provided (use the Read tool on the image file)

### Step 4: Compare with History (optional)
If failures seem like regressions, check previous runs:
```bash
ls $PWD/.automation-results/history/
```

### Step 5: Fix Issues (Use /fix Methodology)
For each identified bug, follow the autonomous fix process:

1. **Locate the relevant code** using error messages, stack traces, network failures as hints
2. **Read the source code** before making changes
3. **Fix the root cause**, not symptoms
4. **Add debug logging** if needed to understand the issue
5. **Restart affected services** after code changes:
   ```bash
   # Use restart-services.sh for service restarts
   $PWD/qontinui-claude-config/scripts/restart-services.sh frontend clean
   $PWD/qontinui-claude-config/scripts/restart-services.sh backend
   ```

6. **For runner issues**, restart the runner:
   ```bash
   powershell.exe -Command "Stop-Process -Name qontinui-runner -Force -ErrorAction SilentlyContinue; Start-Sleep -Seconds 2; cd '$PWD\qontinui-runner'; Start-Process -FilePath 'npm.cmd' -ArgumentList 'run','tauri','dev' -WindowStyle Normal"
   ```

### Step 6: Re-run Automation (REQUIRED if changes were made)
**If any code changes were made, you MUST re-run the automation:**
```
/run-automation <config_path_from_execution.json>
```

The automation will write a follow-up prompt when complete, creating a feedback loop until all tests pass.

**If no issues were found or all tests passed:** Report success and stop.

## Priority Order
Fix issues in this order:
1. Backend API errors (500s, crashes)
2. Frontend runtime errors (console errors, crashes)
3. Runner errors (Rust panics, IPC failures, Python subprocess errors)
4. Missing elements / selector issues
5. Timing / race condition issues
6. Visual / layout issues

## Log Locations (for manual debugging)
If log snapshots aren't available, check live logs:
```bash
BASE="$PWD"

# qontinui-web backend
tail -100 "$BASE/.dev-logs/backend.log"
grep -i error "$BASE/.dev-logs/backend.log" | tail -50

# qontinui-web frontend
tail -100 "$BASE/.dev-logs/frontend.log"
grep -i error "$BASE/.dev-logs/frontend.log" | tail -50

# qontinui-runner's own tracing sink (auth, relay, backend-URL, executor).
# Daily-rolled — read the newest, and glob the runner's app-data dev-logs dir as
# well as the workspace one: everything the runner writes usually lands there.
# Exact dir: GET http://localhost:9876/log-sources/runner-log-sink
# (`runner-tauri.log` is retired as a runner log — it is only stdout capture.)
RDL="$LOCALAPPDATA/qontinui-runner/dev-logs"
RUNNER_LOG=$(ls -t "$BASE"/.dev-logs/qontinui-runner.log.* \
  "$RDL"/qontinui-runner.log.* 2>/dev/null | head -1)
if [ -n "$RUNNER_LOG" ]; then
    tail -100 "$RUNNER_LOG"
else
    echo "NO runner log matched qontinui-runner.log.* in .dev-logs/ or $RDL"
fi

# The supervisor's capture of the primary runner's stdout
tail -100 "$BASE"/.dev-logs/primary.log "$RDL"/primary.log 2>/dev/null

# qontinui-runner EVENT LOGS (JSONL - workflow execution details), both dirs
tail -100 "$BASE"/.dev-logs/runner-general.jsonl "$RDL"/runner-general.jsonl 2>/dev/null
tail -100 "$BASE"/.dev-logs/runner-actions.jsonl "$RDL"/runner-actions.jsonl 2>/dev/null
tail -50 "$BASE"/.dev-logs/runner-image-recognition.jsonl "$RDL"/runner-image-recognition.jsonl 2>/dev/null
tail -50 "$BASE"/.dev-logs/runner-playwright.jsonl "$RDL"/runner-playwright.jsonl 2>/dev/null
tail -100 "$BASE"/.dev-logs/ai-output.jsonl "$RDL"/ai-output.jsonl 2>/dev/null

# Annotated screenshots from image recognition (use Read tool to view)
ls -la "$BASE"/.dev-logs/screenshots/ "$RDL"/screenshots/ 2>/dev/null | tail -20
ls -la "$BASE"/.dev-logs/playwright-screenshots/ "$RDL"/playwright-screenshots/ 2>/dev/null | tail -20

# Last loaded config (use Read tool to view full content)
cat "$BASE/.dev-logs/last-loaded-config.meta.json"
ls -la "$BASE/.dev-logs/last-loaded-config."*
```

**Runner Event Logs** (JSONL format, cleared on startup). These are runner-authored,
so they live in the runner's own dev-logs dir (`$RDL` above) rather than the workspace
`.dev-logs/` unless `paths.dev_logs_dir` is overridden:
- `runner-general.jsonl` - General executor events
- `runner-actions.jsonl` - Workflow execution tree (action logs)
- `runner-image-recognition.jsonl` - Pattern match results with confidence scores
- `runner-playwright.jsonl` - Playwright test results (pass/fail, specs, console output, page snapshots)
- `screenshots/` - Annotated PNG files showing match locations
- `playwright-screenshots/` - Screenshots from Playwright test failures
- `last-loaded-config.*` - Last loaded workflow config file (JSON/YAML)
- `last-loaded-config.meta.json` - Config source path and load timestamp

## Rules
- **ALWAYS** re-run automation after making fixes
- **NEVER** ask the user to run tests or check results manually
- **NEVER** mark a fix as complete without re-running automation
- **NEVER** ask for clarification - make reasonable assumptions and proceed
- Focus on the root cause, not symptoms
- Read source code before making any changes
- Verify services are running before re-running automation

## Arguments
$ARGUMENTS
