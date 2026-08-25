# Recursive Automation Loop

Execute automation steps, analyze results, fix issues, and recursively continue until success.

## Loop Bounds (read before starting)

This loop is **bounded**. Because it recurses by spawning a fresh session per iteration (via
`trigger_ai_analysis`), the loop state must be **threaded through the recursive prompt** — each
iteration receives the prior ledger and iteration count and appends to them.

- **`MAX ITERATIONS = 10`** (unchanged). **Arg-overridable**: if `$ARGUMENTS` contains
  `--max-rounds=N`, use N. Stop after the cap and report remaining issues.
- **Per-round ledger** — each iteration appends one row to a `LEDGER` carried in the recursive
  prompt (see "The Recursive Prompt Template"):
  ```
  round | action | progress-delta | fingerprint | status | ending
  ```
  - `action` — terse: the fixes made this iteration (or "no fixes — all navigations clean").
  - `progress-delta` — error/failure change vs. previous iteration (e.g. `errors 3→1`, `same`, `worse`).
  - `fingerprint` — first 12 hex chars of `sha256( sorted(files_you_edited) + "\n" + error_signature )`,
    where `error_signature` is the current top failing error from logs/screenshots, normalized
    (type + message, stripped of timestamps / paths / pids).
  - `status` — `IN_PROGRESS | SUCCESS | STALL | CAP_REACHED`.
  - `ending` — how THIS iteration's turn ended (see "Turn-ending classification" below):
    `complete | waiting_on_signal | user_deflection | bailout | unknown`. Recorded, not acted on
    automatically.
- **Stall detection (PRIMARY stop)** — if this iteration's `fingerprint` equals the **previous**
  iteration's (same files edited, same failing signal), no progress was made → declare a **STALL**
  and escalate instead of spawning another iteration.
- **Cap (BACKSTOP)** — at iteration 10 (or `--max-rounds=N`) without success, stop and escalate.

### Turn-ending classification

Stall detection watches the loop's **work**; this watches the loop's **prose**, because the one
failure a fingerprint cannot see is the iteration that quietly gives up. The stall rule compares
iteration N against iteration N+1 — and a bail never spawns iteration N+1, so the last ledger row
reads `IN_PROGRESS` forever. **This loop is the worst case for it**: because each iteration is a
FRESH session spawned by `trigger_ai_analysis`, an iteration that ends without recursing simply
never happens again, and there is no live session left to notice.

Judge the ending from the **last non-empty paragraph** of the iteration's final text, matched at its
**start**. The anchoring is the whole trick: an iteration that *discusses* stopping mid-paragraph and
then keeps working is `complete`.

| Ending | Shape |
|---|---|
| `complete` | Does not start with a stop pattern. The overwhelming majority. |
| `waiting_on_signal` | Stops on an **observable** signal with a bounded wait — "resume once the automation run finishes". Legitimate. |
| `user_deflection` | Stops on a **person** — "retry when you approve", "let me know how to proceed". Not a verdict on its own. |
| `bailout` | Stops with neither a signal nor a person to wait on — "I'll stop here", "I am unable to proceed". |
| `unknown` | The iteration's final text could not be read. **Never fold this into `complete`** — count it separately. |

**`user_deflection` is only a bailout when the work is UNGATED.** Policy `planning-and-scope`
`dependency-wait-and-resume` prescribes stopping on a human decision — *provided* the gate and
continuation were registered first ("never end a session with a blocked item that has no registered
gate"). So join the text with gate state: deflection **+ a registered gate** is the prescribed
`stop with status waiting`; deflection **+ no gate** is a bailout. Collapsing that distinction flags
every correctly-closed blocked session, which is how a control this cheap gets switched off.

**What to do with it — nothing automatic.** Record it in `LEDGER` (which is carried forward into the
next iteration's prompt, so the ending survives the session boundary that everything else about the
turn does not) and name it in the handoff. If an iteration is about to end on a `bailout`- or
ungated-`user_deflection`-shaped paragraph, that is the `finish-to-zero` clause telling you the loop
is not done: either spawn the next iteration, or — when the blocker is an **observable** condition —
invoke `/blocked` to register the typed coord gate BEFORE stopping, so it becomes a watched gate
instead of a dead report. Do not implement a re-prompt loop off this verdict; acting on it
automatically is gated behind the runner detector's shadow-corpus review.

**Escalation = `stop_and_report` (default).** On STALL or CAP_REACHED, stop the recursion and emit
the structured handoff (see "Escalation Handoff" below) — do not spawn another iteration, do not
silently give up. Only escalate to an interactive `AskUserQuestion` for: (1) a **security anomaly**,
(2) a fix needing a **coord deploy / web deploy / DB migration**, or (3) a **genuinely-surprising
finding** that makes continuing autonomously reckless.

## Usage

```
/recursive-automation <states_to_visit> [screenshot_locations]
```

Arguments:
- `states_to_visit` - Comma-separated list of states to navigate to (e.g., "StartExtraction,NextStep,Results")
- `screenshot_locations` - (Optional) States where screenshots should be taken for analysis

## Instructions

### Step 1: Ensure Config is Loaded

```bash
# Check current config
python $PWD/qontinui-claude-config/scripts/qontinui-http.py status
```

If no config is loaded, inform the user to load one first with `/run-automation`.

### Step 2: Parse States to Visit

Parse the `states_to_visit` argument into a list. Example: "StartExtraction,NextStep,Results" becomes `["StartExtraction", "NextStep", "Results"]`.

### Step 3: Navigate to Each State

For each state in the list, use the MCP tool to navigate:

```
mcp__qontinui__go_to_state with state_names=["{state}"], take_screenshot=true
```

After each navigation:
1. Check if screenshot was taken
2. Read the screenshot using the Read tool to visually analyze what's on screen
3. If the state has images to click, use `mcp__qontinui__click_image`

### Step 4: Analyze Logs

After completing all state visits, read the relevant logs:

```bash
BASE="$PWD"
# The runner writes its own sink and jsonl streams next to its app-data dir,
# NOT into the workspace .dev-logs/ — read both. See
# qontinui-claude-config/knowledge-base/qontinui-specific/debugging-logs.md
RDL="$LOCALAPPDATA/qontinui-runner/dev-logs"

# The runner's own tracing sink (there is no runner-backend.log — that name
# never existed; verified absent in both dev-logs dirs 2026-07-28)
RUNNER_LOG=$(ls -t "$BASE"/.dev-logs/qontinui-runner.log.* \
  "$RDL"/qontinui-runner.log.* 2>/dev/null | head -1)
if [ -n "$RUNNER_LOG" ]; then
  tail -200 "$RUNNER_LOG"
  grep -i "error\|exception\|failed\|panic" "$RUNNER_LOG" | tail -50
else
  echo "runner log NOT checked - no qontinui-runner.log.* in $BASE/.dev-logs or $RDL"
fi

# Check AI output for any prompts/responses
tail -50 "$BASE"/.dev-logs/ai-output.jsonl "$RDL"/ai-output.jsonl 2>/dev/null
```

### Step 5: Analyze Screenshots

Read any screenshots saved during navigation (typically in `.dev-logs/screenshots/`):

```bash
ls -la "$PWD"/.dev-logs/screenshots/ "$LOCALAPPDATA"/qontinui-runner/dev-logs/screenshots/ 2>/dev/null
```

Use the Read tool on the latest screenshots to visually inspect the UI state.

### Step 6: Identify and Fix Issues

If any errors were found:
1. Identify the root cause from logs and screenshots
2. Read the relevant source code
3. Make the fix
4. Restart affected services if needed:
   ```bash
   $PWD/qontinui-claude-config/scripts/restart-services.sh frontend clean
   ```

### Step 7: Record the Round, Then Continue / Succeed / Escalate

First, compute this iteration's `fingerprint` (see "Loop Bounds") and append a row to `LEDGER`.

Then decide, **in this order** (PRIMARY stops first, BACKSTOP last):

1. **No issues found?** (all states navigated successfully, no errors) → status `SUCCESS`. Report
   success and stop the loop.
2. **Stall?** — this iteration's fingerprint equals the previous iteration's (carried in the
   recursive prompt) → status `STALL`. Stop and emit the Escalation Handoff (below). Do NOT spawn
   another iteration — the same fix on the same files left the same failure.
3. **Cap reached?** — `ITERATION >= 10` (or `--max-rounds=N`) → status `CAP_REACHED`. Stop and emit
   the Escalation Handoff (below).
4. **About to end on a `bailout` or an ungated `user_deflection`?** → you are not done. Spawn the
   next iteration, or register the typed coord gate via `/blocked` if the blocker is observable.
   Never stop here silently — this loop has no live session left to catch it.
5. **Security / coord-deploy-or-migration / surprising finding?** → escalate via `AskUserQuestion`.

Otherwise (**fixes were made, no stall, under the cap**), trigger a new Claude Code session to
continue. Use the `mcp__qontinui__trigger_ai_analysis` tool with the prompt below — carry forward
the updated `LEDGER`, the incremented `ITERATION`, and this iteration's `fingerprint` (so the next
iteration can detect a stall):

```
/recursive-automation {SAME_STATES_LIST}

ITERATION: {iteration_number}
PREV_FINGERPRINT: {this_iteration_fingerprint}
LEDGER (carry forward, append your row):
{ledger_rows_so_far}

Previous iteration made these fixes:
- {LIST_OF_FIXES_MADE}

Continue verifying the automation. If all states navigate successfully with no errors, report success and stop. Otherwise apply the Loop Bounds: append a ledger row (including its `ending` column), detect stall (fingerprint == PREV_FINGERPRINT) and the iteration cap, and escalate per the Escalation Handoff if either fires. Do not end this iteration on a `bailout`- or ungated-`user_deflection`-shaped final paragraph: recurse, or register a coord gate via `/blocked` first.
```

### Escalation Handoff

When you stop on STALL or CAP_REACHED, emit this structured handoff (assembled mechanically from
`LEDGER` — do not re-derive it), then stop:

```
## recursive-automation escalation — <iteration-cap reached | no-progress stall>

- Iterations run: <N> / <MAX>
- States: {states_list}
- Current failing signal: <the error(s)/failed navigations still present, with the error signature>
- Per-round ledger:
  round=1 action=<…> delta=<…> fp=<…> status=<…> ending=<…>
  …
- What was tried each iteration: <one line per iteration — the fix attempted and its outcome>
- Decision needed: <the specific blocker — e.g. "state <X> never resolves; needs a config/image
  change", or "diagnosis handoff: next session should investigate <X>">
```

Do NOT spawn another `trigger_ai_analysis` iteration after emitting the handoff.

## Example

User runs:
```
/recursive-automation StartExtraction,ClickNext,ViewResults StartExtraction,ViewResults
```

This will:
1. Navigate to "StartExtraction" state, take screenshot
2. Navigate to "ClickNext" state
3. Navigate to "ViewResults" state, take screenshot
4. Analyze logs for errors
5. View screenshots from StartExtraction and ViewResults
6. Fix any issues found
7. If fixes made, spawn new Claude to re-run the same automation

## The Recursive Prompt Template

When triggering the next iteration, use this prompt structure:

```
Continue the recursive automation loop.

STATES TO VISIT: {states_list}
SCREENSHOT STATES: {screenshot_states}
ITERATION: {iteration_number}
MAX_ROUNDS: {max_rounds_default_10}
PREV_FINGERPRINT: {previous_iteration_fingerprint}
LEDGER (carry forward, append your row):
{ledger_rows_so_far}
PREVIOUS FIXES: {list_of_fixes_or_none}

Instructions:
1. Navigate to each state using go_to_state MCP tool
2. Take screenshots at specified states
3. Analyze logs for errors after navigation
4. Compute this iteration's fingerprint and append a LEDGER row (round | action | delta | fingerprint | status | ending)
5. Termination (PRIMARY first, BACKSTOP last):
   - No errors → report success and stop
   - fingerprint == PREV_FINGERPRINT → STALL → emit Escalation Handoff and stop (do NOT recurse)
   - ITERATION >= MAX_ROUNDS → CAP_REACHED → emit Escalation Handoff and stop
   - ending is bailout / ungated user_deflection → NOT done: recurse, or register a coord gate via /blocked
   - security / coord-deploy-or-migration / surprising finding → AskUserQuestion
6. Otherwise (errors found, no stall, under cap): fix them, restart services, then trigger another
   iteration via trigger_ai_analysis — carry forward the updated LEDGER, incremented ITERATION, and
   this iteration's fingerprint as PREV_FINGERPRINT.

Use trigger_ai_analysis to spawn the next iteration with this same prompt template.
```

## Rules

- **ALWAYS** analyze logs after navigation
- **ALWAYS** visually inspect screenshots
- **ALWAYS** use trigger_ai_analysis for recursion (NOT write_prompt)
- **ALWAYS** append a ledger row per iteration (including its `ending`) and carry the LEDGER + fingerprint forward in the recursive prompt
- **NEVER** end an iteration on a `bailout`- or ungated-`user_deflection`-shaped final paragraph — recurse, or register a typed coord gate via `/blocked` first. This loop has no live session left to notice a silent stop
- **NEVER** ask the user to check things manually on routine work — the ONLY exceptions are the escalation carve-outs (security anomaly / coord-deploy-or-migration / genuinely-surprising finding), which use `AskUserQuestion`
- **STOP** when all navigations succeed with no errors
- **STALL (PRIMARY stop)**: if an iteration's fingerprint matches the previous iteration's, stop and emit the Escalation Handoff — do NOT spawn another iteration
- **MAX ITERATIONS (BACKSTOP)**: Stop after 10 iterations (arg-overridable via `--max-rounds=N`) and emit the Escalation Handoff with remaining issues

## Arguments

$ARGUMENTS
