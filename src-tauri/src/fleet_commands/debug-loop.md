# Autonomous Debug Loop

Start an autonomous debug loop that fixes errors until all are resolved.

## How Continuation Works

**The runner handles continuation automatically - no file-based state needed!**

1. You analyze logs and fix errors
2. Output `[TASK_COMPLETE]` when all errors are fixed
3. If your session ends early (timeout, context limit):
   - The runner saves your output to the database
   - A new session starts with your previous output as context
   - You review what was done and continue

## Loop Bounds (read before starting)

This loop is **bounded** — it does not run forever. Stall-detection is the PRIMARY stop; the
round cap is the BACKSTOP. Keep an in-memory ledger and check termination after every round.

- **`MAX_ROUNDS = 6`** (a round = one Step 2–4 fix-restart-verify cycle). **Arg-overridable**:
  if `$ARGUMENTS` contains `--max-rounds=N`, use N instead. Generous on purpose — a hard fix
  that legitimately needs several rounds shouldn't bail early.
- **Per-round ledger** — after each round, append one row to an in-memory `LEDGER` string
  (don't write a file):
  ```
  round | action | progress-delta | fingerprint | status | ending
  ```
  - `action` — terse: what you fixed this round.
  - `progress-delta` — error count change vs. previous round (e.g. `errors 5→2`, `same`, `worse`).
  - `fingerprint` — first 12 hex chars of `sha256( sorted(files_you_edited) + "\n" + error_signature )`,
    where `error_signature` is the current top failing error normalized (type + message, stripped
    of timestamps / paths-with-line-numbers / pids).
  - `status` — `IN_PROGRESS | FIXED | STALL | CAP_REACHED`.
  - `ending` — how THIS round's turn ended (see "Turn-ending classification" below):
    `complete | waiting_on_signal | user_deflection | bailout | unknown`. Recorded, not acted on
    automatically.
- **Stall detection (PRIMARY stop)** — if this round's `fingerprint` equals the **previous**
  round's fingerprint (same files edited, same error still failing the same way), you made no
  progress → declare a **STALL** and escalate.
- **Round cap (BACKSTOP)** — if you reach `MAX_ROUNDS` rounds without all errors fixed, stop and
  escalate.

### Turn-ending classification

Stall detection watches the loop's **work**; this watches the loop's **prose**, because the one
failure the fingerprint cannot see is the round where you quietly give up. Element 2's stall rule
compares round N against round N+1 — and a bail ends the loop before round N+1 exists, so the last
ledger row reads `IN_PROGRESS` forever.

Judge the ending from the **last non-empty paragraph** of the round's final text, matched at its
**start**. The anchoring is the whole trick: a round that *discusses* stopping mid-paragraph and
then keeps fixing is `complete`.

| Ending | Shape |
|---|---|
| `complete` | Does not start with a stop pattern. The overwhelming majority. |
| `waiting_on_signal` | Stops on an **observable** signal with a bounded wait — "resume once the backend restart finishes". Legitimate. |
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
`finish-to-zero` clause telling you this loop is not done: either fix the next error, or — when the
blocker is an **observable** condition (a deploy going green, a PR merging, a migration reaching
head) — invoke `/blocked` to register the typed coord gate BEFORE stopping, so the blocker becomes a
watched gate instead of a dead report. Do not implement a re-prompt loop off this verdict; acting on
it automatically is gated behind the runner detector's shadow-corpus review.

**Escalation = `stop_and_report` (default).** On STALL or CAP_REACHED, stop looping and emit the
structured handoff (see "Escalation Handoff" below) — never silently keep looping, never silently
give up. Only escalate to an interactive `AskUserQuestion` for: (1) a **security anomaly** (apparent
credential leak / auth bypass / injection), (2) a fix that needs a **coord deploy / web deploy / DB
migration**, or (3) a **genuinely-surprising finding** that makes continuing autonomously reckless.
For routine cap/stall, just emit the handoff and stop — do not ask the user to restart services or
look at a log.

**Emit-on-block — a stall or a cap is sometimes a BLOCK, not an absence of progress.** Before you
emit the handoff, ask what the loop is actually waiting on. If the remaining error cannot clear
until some **observable** condition does — a deploy or CI run going green, a PR merging, a
migration reaching head, a rebuilt runner becoming the serving build, an upstream fix landing —
then this is not "no progress", it is *blocked on an observable condition*, and you **MUST** invoke
`/blocked` to register the typed coord gate **BEFORE** you stop. That turns the blocker into a
durable, tenant-scoped, fleet-wide watched gate that auto-resumes or notifies on clearance, instead
of a report that dies with this session. Then emit the handoff as usual, naming the registered
`gate_id`. If the blocker has no observable trigger, say so — that case is NOT a gate (see
`/blocked`).

This is **in addition to** the cap/stall handoff, not a replacement, and it is a **separate trigger
from the `bailout` arm above**. That arm cannot cover it: a round that stops on a real signal
classifies as `waiting_on_signal`, which the five-ending table calls *legitimate*, so the bailout
check waves it through by design. Ungated, it is still an unwatched blocked item — the thing
`dependency-wait-and-resume` forbids ending a session on.

## Instructions

### Step 1: Check Logs for Errors

Read the most recent logs:

```bash
BASE="$PWD"

# Backend errors
tail -500 "$BASE/.dev-logs/backend.log" 2>/dev/null | grep -iE "error|exception|traceback|failed" | tail -30

# Frontend errors
tail -500 "$BASE/.dev-logs/frontend.log" 2>/dev/null | grep -iE "error|exception|failed|unhandled" | tail -30

# Runner errors — the runner's own tracing sink is daily-rolled
# (`qontinui-runner.log.<YYYY-MM-DD>`), so resolve the newest file. It usually
# lives in the runner's app-data dev-logs dir, not the workspace .dev-logs/, so
# glob both. Exact dir: GET http://localhost:9876/log-sources/runner-log-sink
RDL="$LOCALAPPDATA/qontinui-runner/dev-logs"
RUNNER_LOG=$(ls -t "$BASE"/.dev-logs/qontinui-runner.log.* \
  "$RDL"/qontinui-runner.log.* 2>/dev/null | head -1)
if [ -n "$RUNNER_LOG" ]; then
    tail -500 "$RUNNER_LOG" | grep -iE "\[ERROR\]|\[WARN\]|Traceback|Exception" | tail -30
else
    echo "NO runner log matched qontinui-runner.log.* in .dev-logs/ or $RDL — runner errors NOT checked"
fi
```

### Step 2: Fix Errors

For each error found:
1. Read the relevant source file
2. Understand the root cause
3. Make the fix
4. Note what you fixed (this becomes context for continuation)

### Step 3: Restart Services

After making fixes, restart affected services:

```bash
BASE="$PWD"

# Restart backend
cd "$BASE" && ./dev-start.ps1 -Backend

# Restart frontend
cd "$BASE" && ./dev-start.ps1 -Frontend

# Wait for services to start
sleep 10
```

### Step 4: Verify Fixes

Re-check logs to see if errors are resolved:

```bash
# Check for new errors after restart
tail -100 "$BASE/.dev-logs/backend.log" | grep -iE "error|exception" | tail -10
tail -100 "$BASE/.dev-logs/frontend.log" | grep -iE "error|exception" | tail -10
```

### Step 5: Record the Round, Then Loop / Complete / Escalate

First, append this round's row to `LEDGER` (compute the fingerprint as described in "Loop Bounds").

Then run the termination checks **in this order** (PRIMARY stops first, BACKSTOP last):

1. **All errors fixed?** → status `FIXED`. Report what was fixed across all rounds and output:
   ```
   [TASK_COMPLETE]
   ```
2. **Blocked on an observable condition?** — the remaining error cannot clear until a deploy/CI run
   goes green, a PR merges, a migration reaches head, or another watchable thing happens → invoke
   `/blocked` to register the typed coord gate **first**, then stop and emit the Escalation Handoff
   naming the `gate_id`. Checked here, ahead of stall and cap, because a block usually *presents* as
   one of them: the same files, the same error, round after round.
3. **Stall?** — this round's fingerprint equals the previous round's → status `STALL`. Stop and
   emit the Escalation Handoff (below). Do not loop again — the same fix on the same files left
   the same error, so another identical round won't help.
4. **Cap reached?** — `round >= MAX_ROUNDS` → status `CAP_REACHED`. Stop and emit the Escalation
   Handoff (below).
5. **About to end on a `bailout` or an ungated `user_deflection`?** → you are not done. Fix the next
   error, or register the typed coord gate via `/blocked` if the blocker is observable. Never stop
   here silently.
6. **Security / coord-deploy / surprising finding?** → escalate via `AskUserQuestion` per the
   carve-outs in "Loop Bounds".

Otherwise (errors remain, no stall, under the cap): go back to Step 2 and fix the next error.

## Escalation Handoff

When you stop on STALL or CAP_REACHED, emit this structured handoff (assembled mechanically from
`LEDGER` — do not re-derive it):

```
## debug-loop escalation — <round-cap reached | no-progress stall | blocked on an observable condition>

- Rounds run: <N> / <MAX_ROUNDS>
- Registered gate: <gate_id from /blocked, or "none — blocker has no observable trigger">
- Current failing signal: <the error(s) still present, with the error signature>
- Per-round ledger:
  round=1 action=<…> delta=<…> fp=<…> status=<…> ending=<…>
  round=2 action=<…> delta=<…> fp=<…> status=<…> ending=<…>
  …
- What was tried each round: <one line per round — the fix attempted and its outcome>
- Decision needed: <the specific blocker — e.g. "error needs a schema change / external
  dependency", or "diagnosis handoff: next session should investigate <X>">
```

Then stop. Do NOT output `[TASK_COMPLETE]` — the loop did not succeed.

## Rules

- **NEVER ask for user input on routine work** - make reasonable decisions autonomously. The ONLY
  exceptions are the escalation carve-outs (security anomaly / coord-deploy-or-migration / genuinely-
  surprising finding), which use `AskUserQuestion`.
- **ALWAYS use tail** for logs (not cat) - you want recent errors, not history
- **Report clearly** what was fixed in your output (this becomes context for continuation)
- **Output `[TASK_COMPLETE]`** only when ALL errors are fixed — never on a STALL or CAP_REACHED stop
- **The loop is bounded** — record a ledger row per round, stop on stall (PRIMARY) or `MAX_ROUNDS`
  (BACKSTOP), and emit the Escalation Handoff rather than looping silently or giving up silently
- **NEVER stop on an observable blocker without registering a gate** — if the error can't clear
  until a deploy goes green, a PR merges or a migration reaches head, run `/blocked` to register the
  typed coord gate BEFORE stopping, and name the `gate_id` in the handoff. The `bailout` check will
  not catch this one: waiting on a real signal is `waiting_on_signal`, which is legitimate — what
  makes it a defect is stopping there ungated
- **Classify how each round ENDED** (`ending` column) — a `bailout` or an ungated `user_deflection`
  means the loop is not done; finish the item or register a coord gate via `/blocked`

## Arguments

$ARGUMENTS
