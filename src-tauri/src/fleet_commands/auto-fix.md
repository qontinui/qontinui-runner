# Auto-Fix Errors from Logs

Check all development logs for errors and fix them autonomously.

## Instructions

1. **Read the most recent logs** (use tail, not cat):

```bash
BASE="$PWD"

# Backend errors (last 500 lines)
tail -500 "$BASE/.dev-logs/backend.log" 2>/dev/null | grep -iE "error|exception|traceback|failed|critical" | tail -30
tail -200 "$BASE/.dev-logs/backend.err.log" 2>/dev/null

# Frontend errors (last 500 lines)
tail -500 "$BASE/.dev-logs/frontend.log" 2>/dev/null | grep -iE "error|exception|failed|type error|unhandled" | tail -30
tail -200 "$BASE/.dev-logs/frontend.err.log" 2>/dev/null

```

2. **For each error found**:
   - Identify the source file from the stack trace
   - Read the relevant source code
   - Determine the root cause
   - Implement a fix

3. **After fixing**, restart the affected service:
```powershell
# From Windows PowerShell
# Restart backend (run from project root)
.\dev-start.ps1 -Backend

# Restart frontend
.\dev-start.ps1 -Frontend
```

4. **Verify the fix** by checking logs again after restart

## Rules

- Work completely autonomously - do NOT ask for clarification
- Fix the root cause, not symptoms
- Always restart the service after code changes
- Always verify by checking logs after restart
- If an error can't be fixed (third-party, needs schema change), skip it and explain why

## Begin

Check the logs now and fix all errors found.
