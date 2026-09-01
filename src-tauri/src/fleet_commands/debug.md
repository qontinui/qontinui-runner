# Debug Command

You are debugging a Qontinui project issue.

## Step 1: Read Context

First, understand the project:
- Read the main CLAUDE.md for general Qontinui context
- Read the project-specific CLAUDE.md if it exists (in the repo being debugged)
- Check knowledge-base/qontinui-specific/common-pitfalls.md for known issues
- Check knowledge-base/debugging/errors.md for common error patterns

## Step 2: Check Logs

**IMPORTANT: ALWAYS check logs first!**

### For qontinui-web:
```bash
# Check the logs written by scripts/dev-start.ps1
cat $PWD/.dev-logs/backend.log | tail -100
cat $PWD/.dev-logs/frontend.log | tail -100

# Check for errors
grep -i error $PWD/.dev-logs/backend.log | tail -50
grep -i error $PWD/qontinui-web/backend/logs/app.log | tail -50

# Backend structured logs (JSON format)
cat $PWD/qontinui-web/backend/logs/app.log | tail -100
```

### For qontinui-runner (Tauri app):
```bash
# The runner's own tracing sink (auth/Cognito, device-JWT/relay,
# backend-URL resolution, executor). Daily-rolled with 14-file retention, so
# resolve the newest — and glob the runner's app-data dev-logs dir as well as
# the workspace one, because that is usually where it actually writes.
# Exact dir: GET http://localhost:9876/log-sources/runner-log-sink
# (`runner-tauri.log` is retired as a runner log — it is only stdout capture.)
RDL="$LOCALAPPDATA/qontinui-runner/dev-logs"
RUNNER_LOG=$(ls -t "$PWD"/.dev-logs/qontinui-runner.log.* \
  "$RDL"/qontinui-runner.log.* 2>/dev/null | head -1)
if [ -n "$RUNNER_LOG" ]; then
    tail -100 "$RUNNER_LOG"

    # Search for errors/warnings in runner
    # Runtime error patterns: [ERROR], [WARNING], Traceback, AttributeError, panic
    grep -i -E "\[ERROR\]|\[WARNING\]|error|warning|panic|Traceback" "$RUNNER_LOG" | tail -50
else
    echo "NO runner log matched qontinui-runner.log.* in .dev-logs/ or $RDL — runner NOT checked"
fi

# The supervisor's capture of the primary runner's stdout
tail -100 "$PWD"/.dev-logs/primary.log "$RDL"/primary.log 2>/dev/null

# Python WebSocket debug log (for runner WebSocket connection issues)
cat $PWD/.dev-logs/python-ws-debug.log
```

**Note:** qontinui-runner logs are automatically captured when started via `dev-start.ps1 -Runner`.

Log file locations:
- qontinui-web backend (dev-start.ps1): `.dev-logs/backend.log`
- qontinui-web frontend (dev-start.ps1): `.dev-logs/frontend.log`
- qontinui-runner's own tracing sink: `qontinui-runner.log.<YYYY-MM-DD>` — daily-rolled, so glob and take the newest, and look in **both** `.dev-logs/` and `<LOCALAPPDATA>/qontinui-runner/dev-logs/` (the runner usually writes to the latter; exact path via `GET http://localhost:9876/log-sources/runner-log-sink`)
- qontinui-runner stdout, captured by the supervisor: `primary.log` (per-runner: `<runner_id>.log`), same two directories
- Supervisor's own tracing: `.dev-logs/supervisor.log`
- Python WebSocket debug: `.dev-logs/python-ws-debug.log`
- Backend (structured): `qontinui-web/backend/logs/app.log`

For other projects, check their respective log locations.

## Step 3: Gather Evidence

Ask the user for:
- Error message and full stack trace
- Steps to reproduce
- Expected vs actual behavior
- When did this start happening?

Check recent changes:
```bash
git log --oneline -20
git diff HEAD~5..HEAD
```

## Step 4: Form Hypotheses

Based on error type, check knowledge base:

### Common Error Patterns
- **TypeError/AttributeError**: Recent refactorings, API changes, missing null checks
  - See: knowledge-base/debugging/errors.md → "TypeError patterns"
- **Integration errors**: Cross-component contracts, version mismatches
  - See: knowledge-base/debugging/integration.md
- **Performance issues**: N+1 queries, inefficient loops, missing memoization
  - See: knowledge-base/debugging/performance.md
- **Import/dependency errors**: Package versions, circular imports
  - Check: `poetry show`, `npm list`

### Qontinui-Specific Issues
- **Workflow vs Process terminology**: Old code might use "processes"
- **Log checking forgotten**: Always check qontinui-web logs first
- **Server restart needed**: Changes not reflected due to cache
- **Poetry lock drift**: Dependencies out of sync

See: knowledge-base/qontinui-specific/common-pitfalls.md

## Step 5: Test Hypotheses Systematically

For each hypothesis:
1. Identify the specific code location
2. Read the relevant files
3. Look for the suspected issue
4. Check `git blame` for recent changes
5. Verify or reject hypothesis

Use binary search to narrow down:
- If recent regression, bisect git commits
- If specific to one component, isolate it
- If environment-specific, compare configs

## Step 6: Root Cause Analysis

Once identified:
- Explain clearly what went wrong and why
- Why it happened (architecture issue, oversight, etc.)
- Why tests didn't catch it (if applicable)
- Document in knowledge base if it's a new pattern

## Step 7: Implement Fix with Comprehensive Logging

**CRITICAL: Add logging that verifies successful implementation**

When implementing fixes:

### 1. Add Structured Debug Logging
```python
# Python example
import logging
logger = logging.getLogger(__name__)

def fixed_function(param):
    logger.debug(f"[FIX_VERIFICATION] Entering fixed_function with param={param}")

    # Your fix here
    result = process_param(param)

    logger.info(f"[FIX_VERIFICATION] Successfully processed: {result}")
    return result
```

```typescript
// TypeScript example
console.log('[FIX_VERIFICATION] Component rendered with props:', props);

// After fix
console.log('[FIX_VERIFICATION] Fix applied successfully, result:', result);
```

### 2. Add Success Markers in Logs
Use the tag `[FIX_VERIFICATION]` for logs that verify your fix works:
- `[FIX_VERIFICATION] Entry point reached`
- `[FIX_VERIFICATION] Validation passed`
- `[FIX_VERIFICATION] Expected behavior achieved`
- `[FIX_VERIFICATION] Fix confirmed working`

### 3. Write Regression Test First
```python
def test_bug_fix_issue_description():
    """Test that verifies the fix for [describe bug]."""
    # Test should fail before fix, pass after fix
    result = fixed_function(test_input)
    assert result == expected_output
    # Log verification
    assert "[FIX_VERIFICATION]" in captured_logs
```

### 4. Implementation Steps
1. Write regression test first (should fail)
2. Implement fix with `[FIX_VERIFICATION]` logging
3. Run tests autonomously
4. Restart servers if needed (do this yourself, don't ask user)
5. Verify fix works
6. Ask user to test manually

## Step 8: Verify Implementation from Logs

After user tests manually, verify success by checking logs:

### For qontinui-web:
```bash
# Check for FIX_VERIFICATION markers
grep FIX_VERIFICATION $PWD/.dev-logs/backend.log | tail -50
grep FIX_VERIFICATION $PWD/.dev-logs/frontend.log | tail -50
grep FIX_VERIFICATION $PWD/qontinui-web/backend/logs/app.log | tail -50

# Check for errors after the fix
grep -i error $PWD/.dev-logs/backend.log | tail -50
grep -i error $PWD/qontinui-web/backend/logs/app.log | tail -50
```

### For qontinui-runner WebSocket issues:
```bash
# Check Python WebSocket debug log
cat $PWD/.dev-logs/python-ws-debug.log

# Check the runner's tracing sink (contains Python executor errors).
# Daily-rolled — take the newest match, across both dev-logs dirs.
RDL="$LOCALAPPDATA/qontinui-runner/dev-logs"
RUNNER_LOG=$(ls -t "$PWD"/.dev-logs/qontinui-runner.log.* \
  "$RDL"/qontinui-runner.log.* 2>/dev/null | head -1)
if [ -n "$RUNNER_LOG" ]; then tail -50 "$RUNNER_LOG"; else echo "NO runner log matched qontinui-runner.log.* in .dev-logs/ or $RDL"; fi
```

### Verification Criteria:
✓ `[FIX_VERIFICATION]` markers appear in logs
✓ No errors related to the fixed issue
✓ Expected behavior logged
✓ Tests pass

### Report to User:
```
Fix verified! From the logs I can see:
✓ [FIX_VERIFICATION] Entry point reached at [timestamp]
✓ [FIX_VERIFICATION] Validation passed at [timestamp]
✓ [FIX_VERIFICATION] Expected behavior achieved
✓ No errors in the last 50 log entries
✓ All tests passing

The fix is confirmed working.
```

## Step 9: Document New Patterns

If this is a new error pattern:
- Update knowledge-base/debugging/errors.md with:
  - Error message
  - Root cause
  - Fix approach
  - Prevention strategy
- Update knowledge-base/qontinui-specific/common-pitfalls.md if Qontinui-specific

## Step 10: Cleanup (Optional)

After user confirms fix works in production:
- Remove or reduce verbosity of `[FIX_VERIFICATION]` logs
- Keep essential logging for monitoring
- Document the fix in comments if complex

## Autonomous Operation

**Work autonomously. Do NOT ask the user to:**
- Restart servers (do it yourself)
- Check logs (you can read them)
- Run tests (you can run them)
- Verify the fix (check logs yourself)

Only ask the user to:
- Provide initial error details
- Test the fix manually (for real-world verification)
- Confirm if they want verbose logs removed after verification
