# Reflect on UI Bridge Usage

Analyze how the UI Bridge was used (or not used) in the most recent workflow execution — including both deterministic UI Bridge steps and AI-driven UI Bridge interactions. Identify gaps, friction points, and missed opportunities. Design concrete improvements and **implement them directly**.

**This command is triggered automatically after every dev-mode workflow run, or can be run manually.**

## Instructions

**CRITICAL**: This command is FULLY AUTONOMOUS. Do NOT ask the user any questions. Analyze all data, produce findings, design improvements, and **implement them**. This is not a read-only analysis — you must make code changes.

---

## Phase 1: Gather Workflow Execution Data

### Step 1.1: Load the AI Conversation Output

The AI conversation from the workflow contains the most important data — it shows exactly what the AI tried, what it struggled with, and where the UI Bridge helped or hindered.

The runner's SQLite DB lives under the CURRENT Windows user's roaming profile —
`%APPDATA%/com.qontinui.runner/runner.db` — so every snippet below resolves it
from the environment. Never hardcode `C:/Users/<someone>/AppData/...`: on a
machine with a different Windows account that path does not exist, and a plain
`sqlite3.connect()` would CREATE an empty database there and report zero rows
instead of failing. Each snippet therefore opens the DB **read-only**
(`?mode=ro`) and prints a visible `RUNNER_DB_MISSING` line, so an absent DB
reads as UNKNOWN rather than as "no workflow data". The URI is built with
`Path.as_uri()`, not string concatenation — a `#` in the account name would
otherwise truncate the path and reintroduce the same silent-empty read.

```bash
BASE="$PWD"

# Get the most recent task run ID
LATEST_RUN=$(python -c "
import json, os, sqlite3, sys
from pathlib import Path
db = Path(os.environ.get('APPDATA') or os.path.expanduser('~/AppData/Roaming')) / 'com.qontinui.runner' / 'runner.db'
if not db.exists():
    print('RUNNER_DB_MISSING: ' + str(db)); sys.exit(0)
conn = sqlite3.connect(db.as_uri() + '?mode=ro', uri=True)
row = conn.execute('SELECT id, workflow_name, status, prompt FROM task_runs ORDER BY created_at DESC LIMIT 1').fetchone()
if row:
    print(json.dumps({'id': row[0], 'workflow_name': row[1], 'status': row[2], 'prompt': row[3][:500] if row[3] else None}))
else:
    print('RUNNER_DB_EMPTY: no rows in task_runs')
conn.close()
" 2>/dev/null)
echo "$LATEST_RUN"
```

```bash
# Get the AI output from the most recent completed run
TASK_ID=$(echo "$LATEST_RUN" | python -c "import sys,json; print(json.load(sys.stdin)['id'])" 2>/dev/null)

# Try live API first (works for running tasks)
curl -s "http://localhost:9876/task-runs/$TASK_ID/output?tail_chars=50000" 2>/dev/null | head -c 50000

# Fallback: read from database (completed tasks)
python -c "
import json, os, sqlite3, sys
from pathlib import Path
db = Path(os.environ.get('APPDATA') or os.path.expanduser('~/AppData/Roaming')) / 'com.qontinui.runner' / 'runner.db'
if not db.exists():
    print('RUNNER_DB_MISSING: ' + str(db)); sys.exit(0)
conn = sqlite3.connect(db.as_uri() + '?mode=ro', uri=True)
rows = conn.execute(\"\"\"
    SELECT event_data FROM task_run_events
    WHERE task_run_id = '$TASK_ID' AND event_type = 'ai_output'
    ORDER BY created_at
\"\"\").fetchall()
for row in rows:
    print(row[0][:10000] if row[0] else '')
conn.close()
" 2>/dev/null
```

### Step 1.2: Load Workflow Definition

```bash
# Get the workflow steps and configuration
python -c "
import json, os, sqlite3, sys
from pathlib import Path
db = Path(os.environ.get('APPDATA') or os.path.expanduser('~/AppData/Roaming')) / 'com.qontinui.runner' / 'runner.db'
if not db.exists():
    print('RUNNER_DB_MISSING: ' + str(db)); sys.exit(0)
conn = sqlite3.connect(db.as_uri() + '?mode=ro', uri=True)
row = conn.execute(\"\"\"
    SELECT name, steps_json, verification_steps_json FROM unified_workflows
    ORDER BY updated_at DESC LIMIT 1
\"\"\").fetchone()
if row:
    print(json.dumps({'name': row[0], 'steps': row[1][:5000] if row[1] else None, 'verification': row[2][:5000] if row[2] else None}, indent=2))
conn.close()
" 2>/dev/null
```

### Step 1.3: Load Step Execution Results

```bash
# Get step-by-step execution results (includes UI Bridge steps)
python -c "
import json, os, sqlite3, sys
from pathlib import Path
db = Path(os.environ.get('APPDATA') or os.path.expanduser('~/AppData/Roaming')) / 'com.qontinui.runner' / 'runner.db'
if not db.exists():
    print('RUNNER_DB_MISSING: ' + str(db)); sys.exit(0)
conn = sqlite3.connect(db.as_uri() + '?mode=ro', uri=True)
rows = conn.execute(\"\"\"
    SELECT step_name, step_type, status, result_json, error_message
    FROM workflow_step_checkpoints
    WHERE task_run_id = (SELECT id FROM task_runs ORDER BY created_at DESC LIMIT 1)
    ORDER BY created_at
\"\"\").fetchall()
results = []
for row in rows:
    results.append({
        'step': row[0], 'type': row[1], 'status': row[2],
        'result': row[3][:2000] if row[3] else None,
        'error': row[4]
    })
print(json.dumps(results, indent=2))
conn.close()
" 2>/dev/null
```

### Step 1.4: Load Runner Action Logs (UI Bridge Interactions)

```bash
BASE="$PWD"
# The runner writes these jsonl streams next to its own sink in app-data, NOT
# into the workspace .dev-logs/ — read both. See
# qontinui-claude-config/knowledge-base/qontinui-specific/debugging-logs.md
RDL="$LOCALAPPDATA/qontinui-runner/dev-logs"

# UI Bridge specific actions from runner logs
grep -i "ui.bridge\|ui_bridge\|snapshot\|discover\|element\|control/" "$BASE"/.dev-logs/runner-actions.jsonl "$RDL"/runner-actions.jsonl 2>/dev/null | tail -100

# General executor events mentioning UI Bridge
grep -i "ui.bridge\|ui_bridge\|snapshot\|discover" "$BASE"/.dev-logs/runner-general.jsonl "$RDL"/runner-general.jsonl 2>/dev/null | tail -50

# AI output logs — look for UI Bridge usage patterns
grep -i "ui.bridge\|snapshot\|discover\|element\|control/" "$BASE"/.dev-logs/ai-output.jsonl "$RDL"/ai-output.jsonl 2>/dev/null | tail -100
```

### Step 1.5: Load Automation Results (if available)

```bash
BASE="$PWD"

# Read execution results
cat "$BASE/.automation-results/latest/execution.json" 2>/dev/null

# Check for screenshots
ls "$BASE/.automation-results/latest/screenshots/" 2>/dev/null
ls "$BASE"/.dev-logs/screenshots/ "$LOCALAPPDATA"/qontinui-runner/dev-logs/screenshots/ 2>/dev/null | tail -20
```

### Step 1.6: Load Browser Events (UI Bridge Client-Side)

```bash
BASE="$PWD"

# Browser events captured by UI Bridge SDK
tail -200 "$BASE/.dev-logs/browser-events.jsonl" 2>/dev/null

# Console errors (may reveal UI Bridge SDK issues)
curl -s https://qontinui.io/api/ui-bridge/control/console-errors 2>/dev/null
curl -s http://localhost:9876/ui-bridge/control/console-errors 2>/dev/null
```

---

## Phase 2: Analyze UI Bridge Usage Patterns

Read through ALL the data gathered in Phase 1 carefully. For each piece of evidence, answer the following questions:

### 2.1: UI Bridge Utilization Map

Create a map of how the UI Bridge was used vs. could have been used:

| Workflow Moment | UI Bridge Used? | How It Was Used | Could It Have Been Used Better? |
|-----------------|----------------|-----------------|--------------------------------|
| (fill from data) | Yes/No/Partially | (describe) | (describe opportunity) |

### 2.2: Core Analysis Questions

Answer each of these questions with specific evidence from the workflow data:

#### Q1: Where did the AI attempt to use the UI Bridge and succeed?
- Which endpoints were called?
- What information was extracted?
- How was that information used to make decisions?
- Was the information sufficient?

#### Q2: Where did the AI attempt to use the UI Bridge and fail?
- What errors or unexpected responses occurred?
- Did the AI retry or fall back to another approach?
- What was the root cause of the failure? (e.g., element not discovered, stale snapshot, wrong element ID, action not supported)
- Could the UI Bridge have prevented the failure with better design?

#### Q3: Where did the AI NOT use the UI Bridge but should have?
- Were there moments where the AI used screenshots, logs, or guesswork instead of querying UI state?
- Did the AI make assumptions about UI state that could have been verified?
- Were there interactions performed via other means (Playwright, image recognition) that the UI Bridge could have handled?
- Did the AI skip UI verification after making code changes?

#### Q4: What information did the AI need but couldn't get from the UI Bridge?
- Did the AI need to understand page structure/layout that the snapshot didn't convey?
- Was there application state (Redux, React context, API cache) that wasn't exposed?
- Did the AI need to understand relationships between elements that weren't expressed?
- Was timing/animation state important but not captured?

#### Q5: What made the UI Bridge hard to use?
- Were element IDs unpredictable or unstable?
- Was the snapshot too large/noisy to parse effectively?
- Were there too many elements, making it hard to find the right one?
- Was the API surface confusing (too many endpoints, unclear which to use)?
- Did the AI struggle with the discover → snapshot → find → act workflow?

#### Q6: What would have made the workflow complete faster or more reliably?
- Fewer round-trips to understand UI state?
- Better element labeling/grouping?
- Higher-level actions (e.g., "fill this form" instead of element-by-element)?
- Better error messages when actions fail?
- Automatic idle/ready detection before snapshots?

### 2.3: Failure Pattern Classification

For each UI Bridge failure or missed opportunity, classify it:

| Pattern | Description | Frequency | Impact |
|---------|-------------|-----------|--------|
| **Discovery gap** | Element exists in DOM but not discovered by UI Bridge | | |
| **Stale snapshot** | Snapshot data doesn't reflect current UI state | | |
| **Element ambiguity** | Multiple elements match, AI picks wrong one | | |
| **Missing state** | Element state doesn't include needed information | | |
| **Action failure** | Action executed but didn't produce expected result | | |
| **API confusion** | AI used wrong endpoint or wrong parameters | | |
| **Missing capability** | UI Bridge simply doesn't support what was needed | | |
| **Performance** | UI Bridge too slow, causing timeout or workflow delay | | |
| **Noise** | Too much data returned, AI overwhelmed or picked wrong signal | | |
| **No UI Bridge** | AI didn't use UI Bridge at all for a task it could have helped with | | |

---

## Phase 3: Implement Quick Wins

Implement improvements that can be completed now:

1. **Prompt/guidance improvements** — Update command markdown files with better patterns
2. **Error message improvements** — Better feedback in SDK or runner
3. **Small SDK changes** — Label improvements, noise reduction, discovery tweaks

After TypeScript changes: `cd ui-bridge && npm run build`
After Rust changes: `cd qontinui-runner/src-tauri && cargo check`

---

## Phase 4: Produce Implementation Plan

For ALL improvements — including architectural changes, new features, multi-file changes, and breaking API changes — produce a structured implementation plan.

**All improvements must align with the UI Bridge Statement of Purpose** (`ui-bridge/STATEMENT_OF_PURPOSE.md`). Read it if you haven't already.

Write the plan as a `[UI_BRIDGE_IMPLEMENTATION_PLAN]` block:

```
[UI_BRIDGE_IMPLEMENTATION_PLAN]
# UI Bridge Improvement Implementation Plan

## Summary
One paragraph describing the overall improvements and expected impact.

## Improvements

### 1. [Improvement Title]
**Category**: SDK / Runner / Prompt / Architecture / New Feature
**Priority**: P0 / P1 / P2
**Evidence**: [Specific reference to workflow data]
**Description**: [2-3 sentences]
**Acceptance Criteria**:
- [ ] [Testable criterion 1]
- [ ] [Testable criterion 2]
**Files to Modify**:
- `path/to/file.ts` — [what changes]
**Verification Method**: command / ui_bridge / test / manual
**Verification Command**: [Shell command to verify, if applicable]

### 2. [Next Improvement]
...

## Testing Strategy
How to verify all improvements work together.

## Dependencies
Ordering constraints between improvements.
[/UI_BRIDGE_IMPLEMENTATION_PLAN]
```

Include BOTH quick wins already implemented (with "Status: Implemented") AND larger improvements.

---

## Phase 5: Generate Implementation Workflow

Use the workflow generator to create a follow-up workflow from the implementation plan:

```bash
curl -s -X POST 'http://localhost:9876/unified-workflows/generate-async' \
  -H 'Content-Type: application/json' \
  -d '{
    "description": "<FULL IMPLEMENTATION PLAN TEXT>",
    "category": "ui-bridge-improvement",
    "tags": ["ui-bridge", "reflection", "auto-generated"],
    "generate_specification": true,
    "verification_depth": "thorough",
    "investigation_codebase": true,
    "reflection_mode": true,
    "include_ui_bridge_instructions": true,
    "discover_ui_bridge_specs": true,
    "auto_run": true,
    "inline_context": "This workflow implements UI Bridge improvements. Each improvement has acceptance criteria. On completion, run /review-plan to review the implementation."
  }'
```

If the generator fails, write the plan to
`$QONTINUI_PLANS_DIR/ui-bridge-implementation-plan.md`. `$QONTINUI_PLANS_DIR` is the
directory plans live in, injected by the qontinui runner from its `paths.plans_dir`
setting; **if it is unset** — a session launched outside the runner will not have it —
ask the user once where plans live, or fall back to `<workspace-root>/plans`. Never
assume an absolute path from another machine.

---

## Phase 6: Save Report

```bash
TIMESTAMP=$(date +%Y%m%d-%H%M%S)
```

Write the full report (Phases 2-5) to `$PWD/.dev-logs/ui-bridge-reflection-$TIMESTAMP.md`.

---

## Phase 7: Summary Report

```markdown
# UI Bridge Reflection Complete

## Workflow Analyzed
- **Name**: {workflow_name}
- **Status**: {success/failed}

## UI Bridge Usage Summary
- **Total UI Bridge calls**: {count}
- **Successful / Failed / Missed opportunities**: {counts}

## Quick Wins Implemented
1. {fix 1}
2. {fix 2}

## Implementation Workflow Generated
- **Task Run ID**: {id}
- **Improvements planned**: {count} (P0: {n}, P1: {n}, P2: {n})
- **Auto-run**: yes/no

## Full report: .dev-logs/ui-bridge-reflection-{timestamp}.md
```

---

## Rules

- **ALWAYS** read the full AI conversation output — this is the primary data source
- **ALWAYS** look for BOTH failures AND missed opportunities
- **NEVER** propose vague improvements — every proposal must reference specific evidence
- **NEVER** skip the implementation plan — it feeds the implementation workflow
- **ALL improvements get implemented** — via quick wins or the generated implementation workflow
- **ALIGN** all improvements with the UI Bridge Statement of Purpose
- **FOCUS** on what makes the AI more effective
- **BE HONEST** — if the UI Bridge worked well, say so. Don't invent problems.

---

## Arguments

$ARGUMENTS
