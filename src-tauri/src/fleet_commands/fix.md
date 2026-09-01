# Autonomous Bug Fix

Work completely autonomously to fix the bug described. Do not ask for clarification or user intervention.

## Steps (Follow in Order)

### Step 1: Check Logs

**CRITICAL: Always read the MOST RECENT logs first.** Old logs may contain errors that have already been fixed. Use `tail` to read from the END of files.

```bash
BASE="$PWD"

# === QONTINUI-WEB BACKEND LOGS ===
# Most recent 200 lines from backend
tail -200 "$BASE/.dev-logs/backend.log" 2>/dev/null
tail -100 "$BASE/.dev-logs/backend.err.log" 2>/dev/null

# Backend errors only (from last 500 lines)
tail -500 "$BASE/.dev-logs/backend.log" 2>/dev/null | grep -iE "error|exception|traceback|failed|critical" | tail -50
tail -500 "$BASE/qontinui-web/backend/logs/app.log" 2>/dev/null | grep -iE '"level":\s*"(error|warning|critical)"' | tail -50

# === QONTINUI-WEB FRONTEND LOGS ===
# Most recent 200 lines from frontend
tail -200 "$BASE/.dev-logs/frontend.log" 2>/dev/null
tail -100 "$BASE/.dev-logs/frontend.err.log" 2>/dev/null

# Frontend errors only (from last 500 lines)
tail -500 "$BASE/.dev-logs/frontend.log" 2>/dev/null | grep -iE "error|exception|failed|unhandled|rejected" | tail -50

# === QONTINUI-RUNNER LOGS ===
# The runner's own tracing sink is `qontinui-runner.log.<YYYY-MM-DD>` — daily-rolled
# with 14-file retention, so resolve the NEWEST match. It usually lives in the
# runner's app-data dev-logs dir rather than the workspace .dev-logs/, so glob
# both. Exact dir: GET http://localhost:9876/log-sources/runner-log-sink
# (`runner-tauri.log` is retired as a runner log — it is only stdout capture.)
# Runtime error patterns: [ERROR], [WARNING], Traceback, AttributeError
RDL="$LOCALAPPDATA/qontinui-runner/dev-logs"
RUNNER_LOG=$(ls -t "$BASE"/.dev-logs/qontinui-runner.log.* \
  "$RDL"/qontinui-runner.log.* 2>/dev/null | head -1)
if [ -n "$RUNNER_LOG" ]; then
    tail -500 "$RUNNER_LOG"

    # Runner runtime errors only (from last 1000 lines) - these are the bugs!
    tail -1000 "$RUNNER_LOG" | grep -iE "\[ERROR\]|\[WARNING\]|\[WARN\]|Traceback|AttributeError|TypeError|Exception|panic" | tail -50
else
    echo "NO runner log matched qontinui-runner.log.* in .dev-logs/ or $RDL — runner NOT checked"
fi
tail -100 "$BASE/.dev-logs/runner.err.log" 2>/dev/null

# The supervisor's capture of the primary runner's stdout (build output lands here)
tail -200 "$BASE"/.dev-logs/primary.log "$RDL"/primary.log 2>/dev/null
# The supervisor's own tracing
tail -100 "$BASE/.dev-logs/supervisor.log" 2>/dev/null

# === PYTHON WEBSOCKET DEBUG LOG ===
tail -200 "$BASE/.dev-logs/python-ws-debug.log" 2>/dev/null

# === QONTINUI-RUNNER EVENT LOGS (JSONL - workflow execution) ===
# These contain structured workflow execution data. The runner writes them next
# to its own sink, so read both dev-logs dirs (see $RDL above).
tail -100 "$BASE"/.dev-logs/runner-general.jsonl "$RDL"/runner-general.jsonl 2>/dev/null
tail -100 "$BASE"/.dev-logs/runner-actions.jsonl "$RDL"/runner-actions.jsonl 2>/dev/null
tail -50 "$BASE"/.dev-logs/runner-image-recognition.jsonl "$RDL"/runner-image-recognition.jsonl 2>/dev/null
tail -50 "$BASE"/.dev-logs/runner-playwright.jsonl "$RDL"/runner-playwright.jsonl 2>/dev/null
tail -100 "$BASE"/.dev-logs/ai-output.jsonl "$RDL"/ai-output.jsonl 2>/dev/null

# List annotated screenshots (use Read tool to view these)
ls -la "$BASE"/.dev-logs/screenshots/ "$RDL"/screenshots/ 2>/dev/null | tail -20
ls -la "$BASE"/.dev-logs/playwright-screenshots/ "$RDL"/playwright-screenshots/ 2>/dev/null | tail -20

# === LAST LOADED CONFIG ===
# Read config metadata to see source path
cat "$BASE/.dev-logs/last-loaded-config.meta.json" 2>/dev/null
# Read the actual config file (use Read tool for full content)
ls -la "$BASE/.dev-logs/last-loaded-config."* 2>/dev/null
```

**There are TWO dev-logs directories.** `$RDL` below is the runner's own,
`<LOCALAPPDATA>/qontinui-runner/dev-logs/` — everything the runner writes lands
there, not in the workspace `.dev-logs/`, unless `paths.dev_logs_dir` is
overridden. Resolve it with `GET http://localhost:9876/log-sources/runner-log-sink`
rather than hardcoding either path.

**Log file locations:**
| Service | Log Files |
|---------|-----------|
| qontinui-web backend | `.dev-logs/backend.log`, `.dev-logs/backend.err.log`, `qontinui-web/backend/logs/app.log` (JSON) |
| qontinui-web frontend | `.dev-logs/frontend.log`, `.dev-logs/frontend.err.log` |
| qontinui-runner (own tracing sink) | `qontinui-runner.log.<YYYY-MM-DD>` in `$RDL` or `.dev-logs/` (daily-rolled — glob `qontinui-runner.log.*`, newest wins), `.dev-logs/runner.err.log` |
| qontinui-runner stdout (supervisor capture) | `primary.log` in `$RDL` or `.dev-logs/` (per-runner: `<runner_id>.log`) |
| qontinui-supervisor | `.dev-logs/supervisor.log` |
| Python WS debug | `.dev-logs/python-ws-debug.log` |

**qontinui-runner Event Logs (JSONL - workflow execution details).** Runner-authored,
so look in `$RDL` first and `.dev-logs/` second:
| Log | Path | Contents |
|-----|------|----------|
| General events | `runner-general.jsonl` | Executor events |
| Action logs | `runner-actions.jsonl` | Workflow execution tree |
| Image recognition | `runner-image-recognition.jsonl` | Pattern match results |
| Playwright tests | `runner-playwright.jsonl` | Test results, specs, console output, page snapshots |
| AI output | `ai-output.jsonl` | Claude conversations |
| Annotated screenshots | `screenshots/*.png` | Visual debug images |
| Playwright screenshots | `playwright-screenshots/*.png` | Test failure screenshots |
| Last loaded config | `.dev-logs/last-loaded-config.*` | Config file (JSON/YAML) |
| Config metadata | `.dev-logs/last-loaded-config.meta.json` | Source path and timestamp |

**Pattern guide for the runner log (`qontinui-runner.log.*`) and `primary.log`:**
- `[ERROR]` / `[WARNING]` - Python executor runtime errors (HIGH PRIORITY!)
- `Traceback` - Python stack traces (ALWAYS investigate!)
- `AttributeError`, `TypeError` - Python code bugs (FIX THESE!)
- `panic` - Rust panics (CRITICAL!)
- IGNORE: `Compiling`, `Building`, `Downloading` - just build output

### Step 2: Find or Create Test
- Look for existing test in `qontinui-web/frontend/tests/e2e/`
- If none exists, create one that reproduces the bug
- Test should verify the expected behavior

### Step 3: Run Test
```bash
cd $PWD/qontinui-web/frontend
SKIP_WEB_SERVER=1 npx playwright test <test-file> --project=chromium
```

### Step 4: Analyze Failure
- Read the screenshot from `test-results/` to see actual UI state
- Check test output for specific errors
- Review network logs for failed API calls

### Step 5: Read Related Code
- Page component in `src/app/`
- API routes in `src/app/api/` (frontend) or `backend/app/api/`
- Services and hooks used by the component
- For runner issues: `qontinui-runner/python-bridge/` (Python) or `qontinui-runner/src-tauri/` (Rust)

### Step 6: Make Code Fix
- Fix the root cause, not symptoms
- Add debug console.log statements if needed to understand the issue

### Step 7: Restart Server (if needed)

Use the restart-services.sh script for reliable restarts:

```bash
BASE="$PWD"

# Restart frontend (with cache clean)
"$BASE/qontinui-claude-config/scripts/restart-services.sh" frontend clean

# Restart backend
"$BASE/qontinui-claude-config/scripts/restart-services.sh" backend

# Restart all web services
"$BASE/qontinui-claude-config/scripts/restart-services.sh" all

# Restart runner (if Python code changed)
powershell.exe -Command "Stop-Process -Name qontinui-runner -Force -ErrorAction SilentlyContinue; Start-Sleep -Seconds 2; cd '$PWD\qontinui-runner'; Start-Process -FilePath 'npm.cmd' -ArgumentList 'run','tauri','dev' -WindowStyle Normal"
```

### Step 8: Re-run Test
```bash
SKIP_WEB_SERVER=1 npx playwright test <test-file> --project=chromium
```

### Step 9: Iterate
- If test still fails, go back to Step 4
- Check new logs for different errors (always use `tail` for most recent!)
- Repeat until test passes

### Step 9b: Invalidate Stale Co-occurrence Observations (if component source was edited)

If the fix modified a component source file (`.tsx`, `.jsx`, `.vue`, `.svelte`) and the edit changed any `aria-label`, `role`, element `textContent`, or `data-*` attribute, emit an invalidation call so stale state-machine observations are cleared. Prefer the `spec_id` filter — `fingerprint_pattern` matches fingerprint hashes, not semantic content. Over-invalidation is recoverable within 24 h via `/undo`.

```bash
curl -sS -X POST http://localhost:9876/co-occurrence/invalidate \
  -H 'Content-Type: application/json' \
  -d '{"spec_id": "<spec-id>", "reason": "fix: source edit to <component>", "invalidated_by": "agent:fix"}'
```

### Step 10: Clean Up and Report
- Remove temporary debug logging
- Report: what was wrong, what was fixed, test results

## Rules

- **NEVER** ask the user to restart servers, check logs, or run tests
- **NEVER** ask for clarification - make reasonable assumptions and proceed
- **ALWAYS** verify the fix with a passing test before reporting success
- **Verification tier** (per the `implementation-priorities` memory): user-facing fixes → goal observed ON THE PAGE via UI Bridge, never inferred from API/DB/logs; consumer-free internals → green tests + documented checks suffice
- **ALWAYS** check logs after failures to understand what went wrong
- **ALWAYS** read failure screenshots to see actual UI state
- **ALWAYS** read the MOST RECENT logs (use `tail`, not `head` or `cat`)

## Bug Report

$ARGUMENTS
