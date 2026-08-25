# Autonomous UI Bug Fix

Work completely autonomously to fix the bug described. Use the **UI Bridge SDK** to inspect and interact with the target application, and read logs when helpful. Do not ask for clarification or user intervention.

## Loop Bounds (read before starting)

This is an autonomous fix loop, but it is **bounded** — it does not iterate forever. Stall-detection
is the PRIMARY stop; the round cap is the BACKSTOP. The autonomous posture is unchanged: don't ask
the user for routine things — escalation on cap/stall is a structured *report*, not a question
(except for the narrow carve-outs below).

- **`MAX_ROUNDS = 6`** — a round = one Step 2→10 cycle (inspect → fix → restart → verify). **Arg-
  overridable**: if the bug report / `$ARGUMENTS` contains `--max-rounds=N`, use N. Generous on
  purpose — a hard UI bug that legitimately needs several rounds shouldn't bail early.
- **Per-round ledger** — after each round, append one row to an in-memory `LEDGER` string (don't
  write a file):
  ```
  round | action | progress-delta | fingerprint | status | ending
  ```
  - `action` — terse: what you changed this round.
  - `progress-delta` — how the bug's failing signal changed vs. last round (e.g. `repro 5/5→2/5`,
    `same`, `worse`, `different-error`).
  - `fingerprint` — first 12 hex chars of `sha256( sorted(files_you_edited) + "\n" + error_signature )`,
    where `error_signature` is the current failing signal normalized (the UI-state mismatch or log
    error type + message, stripped of timestamps / element-ids / pids).
  - `status` — `IN_PROGRESS | FIXED | STALL | CAP_REACHED`.
  - `ending` — how THIS round's turn ended (see "Turn-ending classification" below):
    `complete | waiting_on_signal | user_deflection | bailout | unknown`. Recorded, not acted on
    automatically.
- **Stall detection (PRIMARY stop)** — if this round's `fingerprint` equals the **previous** round's
  (same files edited, same failing signal), you made no progress → declare a **STALL** and escalate.
- **Round cap (BACKSTOP)** — if you reach `MAX_ROUNDS` without the bug resolved, stop and escalate.

### Turn-ending classification

Stall detection watches the loop's **work**; this watches the loop's **prose**, because the one
failure a fingerprint cannot see is the round where you quietly give up. The stall rule compares
round N against round N+1 — and a bail ends the loop before round N+1 exists, so the last ledger row
reads `IN_PROGRESS` forever.

Judge the ending from the **last non-empty paragraph** of the round's final text, matched at its
**start**. The anchoring is the whole trick: a round that *discusses* stopping mid-paragraph and
then keeps fixing is `complete`.

| Ending | Shape |
|---|---|
| `complete` | Does not start with a stop pattern. The overwhelming majority. |
| `waiting_on_signal` | Stops on an **observable** signal with a bounded wait — "resume once the frontend rebuild finishes". Legitimate. |
| `user_deflection` | Stops on a **person** — "retry when you approve", "let me know how to proceed". Not a verdict on its own. |
| `bailout` | Stops with neither a signal nor a person to wait on — "I'll stop here", "I am unable to proceed". |
| `unknown` | The round's final text could not be read. **Never fold this into `complete`** — count it separately. |

**`user_deflection` is only a bailout when the work is UNGATED.** Policy `planning-and-scope`
`dependency-wait-and-resume` prescribes stopping on a human decision — *provided* the gate and
continuation were registered first ("never end a session with a blocked item that has no registered
gate"). So join the text with gate state: deflection **+ a registered gate** is the prescribed
`stop with status waiting`; deflection **+ no gate** is a bailout. Collapsing that distinction flags
every correctly-closed blocked session, which is how a control this cheap gets switched off.

**What to do with it — nothing automatic.** Record it in `LEDGER` and name it in the handoff. If a
round is about to end on a `bailout`- or ungated-`user_deflection`-shaped paragraph, that is the
`finish-to-zero` clause telling you this loop is not done: either take the next round, or — when the
blocker is an **observable** condition (a deploy going green, a PR merging, a rebuild completing) —
invoke `/blocked` to register the typed coord gate BEFORE stopping, so the blocker becomes a watched
gate instead of a dead report. Do not implement a re-prompt loop off this verdict; acting on it
automatically is gated behind the runner detector's shadow-corpus review.

**A `ufix` bail has a signature worth naming**: "the fix needs a change I shouldn't make
autonomously". Check it against the carve-outs — if it is not a security anomaly, a
coord-deploy-or-migration, or a genuinely-surprising finding, it is a bailout, not an escalation.

**Escalation = `stop_and_report` (default).** On STALL or CAP_REACHED, stop and emit the structured
handoff (see "Escalation Handoff" near Step 10) — never silently keep looping, never silently give
up. Stay autonomous: do NOT ask the user to restart servers, check logs, or confirm routine fixes.
Only escalate to an interactive `AskUserQuestion` for: (1) a **security anomaly** (apparent credential
leak / auth bypass / injection), (2) a fix that needs a **coord deploy / web deploy / DB migration**,
or (3) a **genuinely-surprising finding** that makes continuing autonomously reckless.

## UI Bridge Architecture

The UI Bridge is an **SDK-based system** — any React app with the `@qontinui/ui-bridge` SDK installed can be discovered and controlled programmatically. No browser extension required.

**Reference:** See `knowledge-base/qontinui-specific/ui-bridge.md` for full API documentation.

**Two applications have the UI Bridge SDK installed:**

| Application | Base URL | Description |
|-------------|----------|-------------|
| **Runner UI** (Tauri) | `http://localhost:9876/ui-bridge/control/*` | Runner's own React frontend, proxied via Tauri IPC |
| **Web frontend** (Next.js) | `https://qontinui.io/api/ui-bridge/control/*` | qontinui-web frontend, direct HTTP |

Both expose the **same endpoints** — the only difference is the base URL.

## UI Bridge Endpoints

All examples use `$BASE` — set it based on which app you're targeting:

```bash
# For Runner UI bugs:
BASE="http://localhost:9876/ui-bridge"

# For Web frontend bugs:
BASE="https://qontinui.io/api/ui-bridge"
```

**Core endpoints:**

```bash
# Discover all elements (CALL THIS FIRST to trigger registration)
curl -s -X POST $BASE/control/discover -H "Content-Type: application/json" -d '{"interactive_only": false}'

# Discover including display:none/visibility:hidden elements (rarely needed)
curl -s -X POST $BASE/control/discover -H "Content-Type: application/json" -d '{"interactive_only": false, "include_hidden": true}'

# Get full UI state snapshot (elements + state)
curl -s $BASE/control/snapshot

# List registered elements
curl -s $BASE/control/elements

# Get specific element details
curl -s $BASE/control/element/<element-id>

# Click an element
curl -s -X POST $BASE/control/element/<element-id>/action -H "Content-Type: application/json" -d '{"action": "click"}'

# Type into an input
curl -s -X POST $BASE/control/element/<element-id>/action -H "Content-Type: application/json" -d '{"action": "type", "params": {"text": "value"}}'

# Clear an input
curl -s -X POST $BASE/control/element/<element-id>/action -H "Content-Type: application/json" -d '{"action": "clear"}'

# Focus an element
curl -s -X POST $BASE/control/element/<element-id>/action -H "Content-Type: application/json" -d '{"action": "focus"}'

# Scroll an off-screen element into view (use before clicking elements in scroll containers)
curl -s -X POST $BASE/control/element/<element-id>/action -H "Content-Type: application/json" -d '{"action": "scrollIntoView"}'

# Find elements by text content (no need to know the ID)
curl -s -X POST $BASE/ai/find -H "Content-Type: application/json" -d '{"query": "Button text", "type": "button"}'

# Monitor UI changes after actions (enable first, then drain)
curl -s -X POST $BASE/ai/change-buffer/enable
curl -s -X POST $BASE/ai/change-buffer/drain

# Subscribe to real-time UI events via SSE (use -N for streaming)
curl -s -N "$BASE/control/events/stream?types=element:stateChanged,action:completed"
```

**Page navigation (web frontend):**

```bash
# Navigate to a URL
curl -s -X POST $BASE/control/page/navigate -H "Content-Type: application/json" -d '{"url": "/build/workflows"}'

# Refresh the page
curl -s -X POST $BASE/control/page/refresh
```

**MCP Tools (if ui-bridge-mcp is configured):**
- `mcp__ui-bridge__ui_snapshot` - Get full UI state
- `mcp__ui-bridge__ui_discover` - Trigger element discovery
- `mcp__ui-bridge__ui_click` - Click an element
- `mcp__ui-bridge__ui_type` - Type into an input
- `mcp__ui-bridge__ui_get_element` - Get element details

## Steps (Follow in Order)

### Step 1: Understand the Bug and Determine Target

Read the bug report carefully. Determine which application is affected:
- **Runner UI** (Tauri webview) → Base: `http://localhost:9876/ui-bridge`
- **Web frontend** (Next.js on port 3001) → Base: `https://qontinui.io/api/ui-bridge`
- **Backend API** (FastAPI on port 8000) → Use logs and direct HTTP calls

### Step 2: Check UI State via UI Bridge

```bash
# Set BASE based on target app (runner or web frontend)

# Discover all elements on the current view
curl -s -X POST $BASE/control/discover -H "Content-Type: application/json" -d '{"interactive_only": false}'

# Get a full snapshot of the UI state (auto-falls back to native screenshot if SDK is down)
curl -s $BASE/control/snapshot

# List all registered elements with their states
curl -s $BASE/control/elements
```

Inspect the UI state to understand what the user currently sees. Look for:
- Missing elements or unexpected element states
- Disabled/hidden elements that should be visible
- Error messages displayed in the UI
- Incorrect data being shown

**If the SDK is not connected** (snapshot returns `"source": "native_capture"` or discover/elements fail with "Frontend did not become ready"):

The snapshot endpoint automatically falls back to a native window capture — check its response for a `screenshot` field (base64 PNG) and `"source": "native_capture"`. This tells you what the webview is actually showing (e.g., `ERR_CONNECTION_REFUSED`, blank screen, crash page) without needing the React SDK. You can also call the native capture endpoint directly:

```bash
# Native window capture — works even when SDK/React is completely dead (Runner only)
curl -s "http://localhost:9876/ui-bridge/control/annotated-screenshot?runner=true"
# Returns: {"success":true,"data":{"screenshot":"<base64 PNG>","width":...,"height":...}}

# Health endpoint also includes a diagnosticScreenshot when ready:false for >30s
curl -s http://localhost:9876/ui-bridge/health
# Look for data.diagnosticScreenshot.screenshot (base64 PNG)
```

Decode the base64 PNG to visually inspect what the user sees. This is critical for diagnosing webview-level issues (connection errors, blank pages) that the SDK can never report because it hasn't loaded.

**Tips:**
- Elements inside scroll containers are discoverable even when scrolled out of view — they appear with `visible: false` and `inViewport: false`. Use `scrollIntoView` action to bring them into the viewport before clicking.
- Use `/ai/find` to search for elements by text: `POST $BASE/ai/find` with `{"query": "text to find"}`
- Look for elements with `data-ui-id` attributes — these are stable automation IDs (e.g., `spec-chat-send`, `spec-tree-{specId}`)
- Use the change buffer to monitor UI state changes: `POST $BASE/ai/change-buffer/enable`, then `POST $BASE/ai/change-buffer/drain`
- Don't filter discovered elements by `visible=true` when looking for items in scroll containers — they'll have `visible=false` but are still interactable after `scrollIntoView`

### Step 3: Check Logs (When Helpful)

If the UI state alone doesn't reveal the issue, check logs for errors:

```bash
BASE_LOGS="$PWD"

# === QONTINUI-WEB BACKEND LOGS ===
tail -200 "$BASE_LOGS/.dev-logs/backend.log" 2>/dev/null
tail -500 "$BASE_LOGS/.dev-logs/backend.log" 2>/dev/null | grep -iE "error|exception|traceback|failed|critical" | tail -50

# === QONTINUI-WEB FRONTEND LOGS ===
tail -200 "$BASE_LOGS/.dev-logs/frontend.log" 2>/dev/null
tail -500 "$BASE_LOGS/.dev-logs/frontend.log" 2>/dev/null | grep -iE "error|exception|failed|unhandled|rejected" | tail -50

# === QONTINUI-RUNNER LOGS ===
# The runner's own tracing sink is daily-rolled (`qontinui-runner.log.<date>`),
# so resolve the NEWEST match — and glob the runner's app-data dev-logs dir as
# well as the workspace one, which is where it usually writes.
# Exact dir: GET http://localhost:9876/log-sources/runner-log-sink
# (`runner-tauri.log` is retired as a runner log — it is only stdout capture.)
RDL="$LOCALAPPDATA/qontinui-runner/dev-logs"
RUNNER_LOG=$(ls -t "$BASE_LOGS"/.dev-logs/qontinui-runner.log.* \
  "$RDL"/qontinui-runner.log.* 2>/dev/null | head -1)
if [ -n "$RUNNER_LOG" ]; then
    tail -500 "$RUNNER_LOG"
    tail -1000 "$RUNNER_LOG" | grep -iE "\[ERROR\]|\[WARNING\]|\[WARN\]|Traceback|AttributeError|TypeError|Exception|panic" | tail -50
else
    echo "NO runner log matched qontinui-runner.log.* in .dev-logs/ or $RDL — runner NOT checked"
fi
# The supervisor's capture of the primary runner's stdout
tail -200 "$BASE_LOGS"/.dev-logs/primary.log "$RDL"/primary.log 2>/dev/null

# === RUNNER EVENT LOGS (JSONL) === (runner-authored: read both dev-logs dirs)
tail -100 "$BASE_LOGS"/.dev-logs/runner-general.jsonl "$RDL"/runner-general.jsonl 2>/dev/null
tail -100 "$BASE_LOGS"/.dev-logs/runner-actions.jsonl "$RDL"/runner-actions.jsonl 2>/dev/null
tail -50 "$BASE_LOGS"/.dev-logs/runner-image-recognition.jsonl "$RDL"/runner-image-recognition.jsonl 2>/dev/null
tail -50 "$BASE_LOGS"/.dev-logs/runner-playwright.jsonl "$RDL"/runner-playwright.jsonl 2>/dev/null
tail -100 "$BASE_LOGS"/.dev-logs/ai-output.jsonl "$RDL"/ai-output.jsonl 2>/dev/null

# === SCREENSHOTS ===
ls -la "$BASE_LOGS"/.dev-logs/screenshots/ "$RDL"/screenshots/ 2>/dev/null | tail -20
ls -la "$BASE_LOGS"/.dev-logs/playwright-screenshots/ "$RDL"/playwright-screenshots/ 2>/dev/null | tail -20
```

**CRITICAL:** Always use `tail` to read from the END of log files. Old entries may contain already-fixed errors.

### Step 4: Reproduce the Bug via UI Bridge

Use the UI Bridge to reproduce the bug by interacting with the application:

```bash
# Navigate to the relevant page/section by clicking navigation elements
curl -s -X POST $BASE/control/element/<nav-item>/action -H "Content-Type: application/json" -d '{"action": "click"}'

# Wait for the UI to settle (DOM mutations, in-flight network, loading indicators,
# form-pending, animations) — far more reliable than a fixed sleep. Then re-discover.
curl -s -X POST $BASE/control/wait-for-idle -H "Content-Type: application/json" -d '{"timeout":5000,"minStableMs":250}'
curl -s -X POST $BASE/control/discover -H "Content-Type: application/json" -d '{"interactive_only": false}'

# If an element is inside a scroll container and not visible, scroll it into view first
curl -s -X POST $BASE/control/element/<element-id>/action -H "Content-Type: application/json" -d '{"action": "scrollIntoView"}'

# Fill in form fields if needed
curl -s -X POST $BASE/control/element/<input-id>/action -H "Content-Type: application/json" -d '{"action": "type", "params": {"text": "test value"}}'

# Click the button/action that triggers the bug
curl -s -X POST $BASE/control/element/<button-id>/action -H "Content-Type: application/json" -d '{"action": "click"}'

# Wait for the UI to settle, then snapshot.
curl -s -X POST $BASE/control/wait-for-idle -H "Content-Type: application/json" -d '{"timeout":5000,"minStableMs":250}'
curl -s $BASE/control/snapshot
```

Observe what happens. Compare expected vs actual behavior.

### Step 5: Read Related Code

Based on the bug and the affected UI elements:
- **Runner UI components:** `qontinui-runner/src/components/`
- **Runner hooks/state:** `qontinui-runner/src/hooks/`, `qontinui-runner/src/stores/`
- **Runner Rust backend:** `qontinui-runner/src-tauri/src/`
- **Web frontend pages:** `qontinui-web/frontend/src/app/`
- **Web API routes:** `qontinui-web/frontend/src/app/api/` or `qontinui-web/backend/app/api/`

### Step 6: Make Code Fix

- Fix the root cause, not symptoms
- Add debug console.log or tracing statements if needed to understand the issue

### Step 7: Restart Server (if needed)

Use dev-start.ps1 for reliable restarts:

```powershell
# From parent directory
# Restart frontend only (run from project root)
.\dev-start.ps1 -Frontend

# Restart backend only
.\dev-start.ps1 -Backend

# Restart web (backend + frontend) — there is no -Web switch; run both,
# or use -All for the whole stack
.\dev-start.ps1 -Backend
.\dev-start.ps1 -Frontend

# Restart runner (if Rust/Python code changed)
.\dev-start.ps1 -Runner
```

Wait 5-10 seconds after restart before testing.

### Step 8: Verify Fix via UI Bridge

After restarting, use the UI Bridge again to verify the fix:

```bash
# Re-discover elements (UI may have changed after restart)
curl -s -X POST $BASE/control/discover -H "Content-Type: application/json" -d '{"interactive_only": false}'

# Reproduce the steps from Step 4
# Verify the expected behavior now occurs

# Take a final snapshot to confirm the fix
curl -s $BASE/control/snapshot
```

### Step 8b: Run Design Audit for Visual Fixes

If the fix involved CSS, styles, colors, contrast, or visual changes, run a design audit to catch accessibility regressions:

```bash
# First, check if the SDK is connected
curl -s $BASE/control/page/evaluate -H 'Content-Type: application/json' -d '{"expression": "true"}'
# Or check SDK status directly:
curl -s http://localhost:9876/ui-bridge/sdk/status
```

**If SDK is connected**, run `runDesignAudit` to check for contrast issues, select option visibility, and accessibility problems:

```bash
# Run design audit via runDesignAudit endpoint
curl -s -X POST $BASE/ai/design-audit -H "Content-Type: application/json"
```

Review the audit results:
- **error** severity issues (contrast ratio <1.15:1 � text nearly invisible): **Must fix before considering the fix complete**
- **warning** severity issues (contrast ratio <3.0:1 � fails WCAG AA): Fix if related to the change
- **info** severity issues (contrast ratio <4.5:1 for normal text): Note for follow-up

Each finding includes a `fix` field with actionable instructions � apply those suggestions.

**If SDK is not connected or unavailable**, fall back to source code inspection:
- Review CSS/style changes for hardcoded colors without sufficient contrast
- Check that `<select>` elements with dark backgrounds have `color-scheme: dark`
- Verify option text colors are explicitly set when using dark themes

### Step 9: Run Tests (if applicable)

If there are relevant tests, run them to ensure no regressions:

```bash
# Playwright tests (web frontend)
cd $PWD/qontinui-web/frontend
SKIP_WEB_SERVER=1 npx playwright test <test-file> --project=chromium

# Rust tests (runner backend)
cd $PWD/qontinui-runner/src-tauri
cargo test

# TypeScript checks (runner frontend)
cd $PWD/qontinui-runner
npm run typecheck
```

### Step 10: Record the Round, Then Iterate / Resolve / Escalate

First, append this round's row to `LEDGER` (compute the fingerprint as described in "Loop Bounds").

Then run the termination checks **in this order** (PRIMARY stops first, BACKSTOP last):

1. **Bug resolved?** (UI Bridge verification in Step 8 confirms the expected behavior) → status
   `FIXED`. Proceed to Step 11 (Clean Up and Report).
2. **Stall?** — this round's fingerprint equals the previous round's → status `STALL`. Stop and emit
   the Escalation Handoff (below). Another identical round won't help: the same edit to the same
   files left the same failing signal.
3. **Cap reached?** — `round >= MAX_ROUNDS` → status `CAP_REACHED`. Stop and emit the Escalation
   Handoff (below).
4. **About to end on a `bailout` or an ungated `user_deflection`?** → you are not done. Take the next
   round, or register the typed coord gate via `/blocked` if the blocker is observable. Never stop
   here silently.
5. **Security / coord-deploy-or-migration / surprising finding?** → escalate via `AskUserQuestion`
   per the carve-outs in "Loop Bounds".

Otherwise (bug not yet resolved, no stall, under the cap): go back to Step 2. Use the UI Bridge to
observe what changed after your fix, and check logs for new/different errors.

#### Escalation Handoff

When you stop on STALL or CAP_REACHED, emit this structured handoff (assembled mechanically from
`LEDGER` — do not re-derive it), then stop:

```
## ufix escalation — <round-cap reached | no-progress stall>

- Bug: <one-line restatement of the bug report>
- Rounds run: <N> / <MAX_ROUNDS>
- Current failing signal: <what the UI Bridge / logs still show vs. expected, with error signature>
- Per-round ledger:
  round=1 action=<…> delta=<…> fp=<…> status=<…> ending=<…>
  round=2 action=<…> delta=<…> fp=<…> status=<…> ending=<…>
  …
- What was tried each round: <one line per round — the fix attempted and its outcome>
- Decision needed: <the specific blocker — e.g. "root cause is in <repo/file> needing a contract
  change", or "diagnosis handoff: next session should investigate <X>">
```

Do this as a *report*, autonomously — do not turn it into a question to the user unless it hits a
carve-out (security / coord-deploy / surprising finding).

### Step 10b: Invalidate Stale Co-occurrence Observations (if component source was edited)

If the fix modified a component source file (`.tsx`, `.jsx`, `.vue`, `.svelte`) and the edit changed any `aria-label`, `role`, element `textContent`, or `data-*` attribute, emit an invalidation call so stale state-machine observations are cleared. Prefer the `spec_id` filter — `fingerprint_pattern` matches fingerprint hashes, not semantic content. Over-invalidation is recoverable within 24 h via `/undo`.

```bash
curl -sS -X POST http://localhost:9876/co-occurrence/invalidate \
  -H 'Content-Type: application/json' \
  -d '{"spec_id": "<spec-id>", "reason": "ufix: source edit to <component>", "invalidated_by": "agent:ufix"}'
```

### Step 11: Clean Up and Report

- Remove temporary debug logging
- Report: what was wrong, what was fixed, how it was verified via UI Bridge

## Troubleshooting: Frozen / Unresponsive App

If UI Bridge commands time out or return errors, the browser tab may be frozen:

```bash
# Check health (web app)
curl -s https://qontinui.io/api/ui-bridge/health | jq '{healthy, heartbeatAgeMs}'

# Check health (runner proxy for SDK app)
curl -s http://localhost:9876/ui-bridge/sdk/status | jq '.data.healthy'
```

If `healthy: false`:
1. **Web app:** Try refreshing the page via `POST $BASE/control/page/refresh`
2. **Runner webview:** The runner should auto-recover, or restart it via `.\dev-start.ps1 -Runner`
3. All command-based endpoints require a responsive browser tab — they will time out if the tab is frozen

## Rules

- **NEVER** declare the bug fixed from backend data alone — the user's stated goal MUST be observed on the actual page via the UI Bridge (`discover`/`snapshot` the page the bug concerns and confirm the expected state renders). API/HTTP responses, DB rows, coord/session/device registration, status endpoints, logs, and heartbeats confirm plumbing, NOT the goal, and routinely disagree with the rendered page (e.g. coord API returns 3 sessions while the Live Sessions page shows "0 sessions"). If the goal is "X shows on page Y," verification = seeing X on page Y through the UI Bridge. If the surface is unreachable (relay down, no connected tab, auth wall), report **UNVERIFIED** — do not substitute a backend check.
- **NEVER** ask the user to restart servers, check logs, or test manually
- **NEVER** ask for clarification on routine work - make reasonable assumptions and proceed. The ONLY
  exceptions are the escalation carve-outs (security anomaly / coord-deploy-or-migration / genuinely-
  surprising finding), which use `AskUserQuestion`.
- **The loop is bounded** - record a ledger row per round; stop on stall (PRIMARY) or `MAX_ROUNDS=6`
  (BACKSTOP, arg-overridable) and emit the Escalation Handoff rather than looping or giving up silently
- **Classify how each round ENDED** (`ending` column) — a `bailout` or an ungated `user_deflection`
  means the loop is not done; take the next round or register a coord gate via `/blocked`
- **ALWAYS** use the UI Bridge to inspect and verify UI state before and after fixes
- **ALWAYS** check logs when the UI state alone doesn't explain the issue
- **ALWAYS** verify the fix by re-checking UI state via the UI Bridge
- **ALWAYS** read the MOST RECENT logs (use `tail`, not `head` or `cat`)
- **ALWAYS** call discover before listing elements (elements may not be registered until discovered)
- **ALWAYS** set the correct BASE URL based on the target application (runner vs web frontend)

## Bug Report

$ARGUMENTS
