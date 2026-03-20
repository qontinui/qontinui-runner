## Runner Architecture & Iteration Bundle System

The qontinui-runner executes GUI automation and spawns AI sessions. All relevant data is bundled and delivered to you automatically - no file searching required.

### CRITICAL: When Pre-Execution Runs

**Pre-execution steps (GUI automation, screenshots, Playwright tests) run at the START of each new iteration, BEFORE your AI session starts.**

The iteration lifecycle is:

1. **Runner starts new iteration** → Runs all pre-execution steps (clicks, screenshots, tests)
2. **Runner captures results** → Creates the Iteration Data Bundle
3. **Runner spawns AI session** → Delivers the bundle to you
4. **AI session works** → You analyze results, make code changes, debug
5. **AI session signals [WORK_COMPLETE]** → System runs verification
6. **System verifies** → Deterministic checks (build, tests) + AI verification (if configured)
7. **If verification fails** → Go to step 1 for next iteration with feedback
8. **If verification passes** → System marks task complete

**Key implications:**

- The Pre-Execution Results you see are from steps that ALREADY RAN
- To get NEW screenshots or automation results, you must END your current iteration
- You CANNOT re-run pre-execution steps mid-iteration - they only run at iteration start
- If you're resuming mid-iteration after a restart, pre-execution will NOT re-run

### Iteration Data Bundle

When your session starts, the runner provides a complete **Iteration Data Bundle** containing everything you need. Look for the `## Iteration N Data Bundle` section in your prompt. It contains:

1. **Pre-Execution Results** - Step-by-step results from automation (ALREADY COMPLETED)
   - Which steps succeeded or failed (with checkmarks/crosses)
   - Error messages for any failures
   - Screenshot paths for each step (use Read tool to view)
   - Duration for each step

2. **Application Logs** - User-defined log sources (e.g., backend.log, frontend.log)
   - Captured ONLY during this iteration
   - Errors and warnings are highlighted at the top
   - Full log content follows

3. **Runner GUI Automation Logs** (if workflow has GUI steps)
   - Image recognition table with template names, confidence, and thresholds
   - Failed matches explained with confidence vs threshold
   - Action timeline in JSON format

4. **Playwright Test Results** (if workflow has Playwright steps)
   - Overall pass/fail status
   - Individual test spec results
   - Error messages for failed tests
   - Failure screenshot paths

5. **Previous Iteration Findings** - Your structured findings from prior iterations
   - Status indicators (resolved, in progress, pending)
   - Helps you avoid repeating work

### Key Principles

- **Everything is bundled** - Don't search log files; data is delivered to you
- **Relevance-filtered** - Only logs relevant to your workflow's step types are included
- **Iteration-scoped** - Only data from the current iteration, not historical noise
- **Source-labeled** - Every piece of data says where it came from
- **Pre-execution is past tense** - Results show what ALREADY happened, not what will happen

### Start Here

1. Find the `## Iteration N Data Bundle` section (near the end of your prompt)
2. Check the Pre-Execution Results table for failures
3. If GUI steps failed, check the Image Recognition table for low confidence matches
4. If tests failed, check Playwright Results for error details
5. Review Application Logs for related errors

### Getting Fresh Screenshots

If you need new screenshots after making code changes:

1. Complete your current work (make fixes, commit if needed)
2. Signal `[WORK_COMPLETE]` to trigger verification (which includes fresh pre-execution)
3. If verification fails, you'll get a new iteration with fresh screenshots and test results
4. Your next AI session will receive updated data in the Iteration Bundle

**Do NOT try to manually trigger screenshots or re-run automation steps** - the system handles this as part of verification.

### Raw Log Files (Fallback Only)

If you need raw files (rare), they're in `.dev-logs/`:

| File                             | Contains              |
| -------------------------------- | --------------------- |
| `runner-general.jsonl`           | Executor events       |
| `runner-actions.jsonl`           | Action execution      |
| `runner-image-recognition.jsonl` | Template matching     |
| `screenshots/`                   | Annotated screenshots |

### Accessing Data

**Your Iteration Bundle contains everything you need.** Don't search for log files - they're already provided.

If you need data NOT in the bundle (rare):

**Raw log files (.dev-logs/):**
| File | What it contains |
|------|------------------|
| `runner-general.jsonl` | Executor events |
| `runner-actions.jsonl` | Action/tree events |
| `runner-image-recognition.jsonl` | Image match results |
| `runner-playwright.jsonl` | Playwright test results |
| `screenshots/*.png` | Annotated screenshots |

**Database (direct file access):**

- Windows: `C:\Users\<USER>\AppData\Roaming\com.qontinui.runner\runner.db`
- macOS/Linux: `~/.config/com.qontinui.runner/runner.db`
- Tables: `task_runs` (status, output, findings), `run_details` (execution data)

**MCP tools (if available in your session):**

- `get_task_runs`, `get_task_run`, `read_runner_logs`, `list_screenshots`
- Note: MCP tools may not be available in all runner-spawned sessions

### What NOT to Do

- Don't search for log files - they're already in your Iteration Bundle
- Don't read source code to understand architecture - use this context
- Don't look at historical data from other sessions - focus on this iteration
- Don't try to re-run pre-execution steps - they already ran at iteration start
- Don't expect new screenshots mid-iteration - end the iteration first
