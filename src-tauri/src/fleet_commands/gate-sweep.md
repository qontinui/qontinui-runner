---
description: Run the plan-gate sweep inline and report open/closed/blocked gates in the console
argument-hint: "[report|auto] [filter-substring]"
allowed-tools: Read, Glob, Grep, Bash, Edit, ToolSearch
---

# Gate Sweep (console)

Evaluate the gates declared across the plans in `$QONTINUI_PLANS_DIR` (see
[Plan directories](#plan-directories)) and report,
**right here in the console**, which ones are now OPEN (ready to proceed), which are
still CLOSED, and which can't be machine-checked. This is the interactive sibling of
the scheduled `QontinuiGateSweep` task. The gate contract is the `GATE-SWEEP:BEGIN`
block embedded in each plan (a minority of plans carry one); there is no separate
contract doc.

## Plan directories

- **`$QONTINUI_PLANS_DIR`** — the directory plans live in, and the directory this
  sweep scans. The qontinui runner injects it into agent sessions from its
  `paths.plans_dir` setting; a session launched outside the runner will not have it.
  **If it is unset, ask the user once where plans live, or fall back to
  `<workspace-root>/plans`** (a `plans/` directory beside the repos this session is
  working in). Never assume an absolute path from another machine, and state the
  directory you actually scanned in the report — a sweep of the wrong corpus reads
  exactly like a sweep with no open gates.

In `auto` mode this command edits plan files in place. If the plan directory is inside
a git repo (`git -C "$QONTINUI_PLANS_DIR" rev-parse --is-inside-work-tree`), commit the
flipped gate blocks by explicit path; if it is not, the edited files on disk are the
whole record — do not create a repo to hold them.

You are running **inline in the current session** (not the headless task), so you have
this session's tools and auth — including coord MCP / coord SQL reachability the
headless sweep lacks. Use that: load coord MCP tools via ToolSearch when a `check`
needs them, and mint a coord Bearer via `_scratch/cognito-login.ps1` (SSM creds) if a
coord HTTP route is auth-gated.

## Arguments — `$ARGUMENTS`

- *(empty)* or `report` → **report-only** (default). Evaluate everything, print the
  report, **make no edits to any plan file**. Safe and predictable.
- `auto` → after reporting, **remediate the OPEN `on_open: auto` gates** within standing
  authorization (`feedback_autonomous_merge_deploy_migrate_checks`: merge=green-CI/no-reap,
  deploy=ok, migrate=single-head) and flip each acted gate to `state: done` with a dated
  `note:`. Still only *notify* (never act on) `on_open: notify` and `MANUAL:` gates.
  Honor every binding memory rule (worktree isolation, verify-on-page, coord clippy gate).
- A trailing **filter substring** (e.g. `report twin`, `auto mtc`) → restrict the sweep to
  plans/gate-ids whose filename or id contains that substring. Useful for a focused check.

## Procedure

1. **Collect gates.** The plans dir can hold hundreds of files while only a few dozen
   carry a gate block, so **Grep first, then read only the hits** — globbing and
   parsing the whole corpus is impractical:
   `Grep "GATE-SWEEP:BEGIN"` over `$QONTINUI_PLANS_DIR` (skip `scout-*`, and
   skip any file whose only `GATE-SWEEP` block is an example).
   In each hit, parse the YAML inside the `<!-- GATE-SWEEP:BEGIN -->…<!-- GATE-SWEEP:END -->`
   block. Consider only `state: pending` gates. Apply the filter substring if given.
   If `$QONTINUI_PLANS_DIR/.gate-sweep/DISABLED` exists, say so and stop.

2. **Evaluate each `check`** → **OPEN** (condition satisfied / ready), **CLOSED** (still
   waiting), or **UNKNOWN** (not machine-checkable from here). Run the check with Bash
   (`gh`, `curl`, `jq`, `aws`, `git`) / coord MCP / coord SQL. A `check` starting with
   `MANUAL:` is UNKNOWN by definition — never auto-act on it. **Be conservative:** if you
   cannot positively confirm OPEN, it is CLOSED/UNKNOWN. Run independent checks in parallel.
   When a check reads coord gate rows: verdict **`withdrawn`** (registrant cancelled its
   own gate — coord PR #1247, landed) is **terminal and non-blocking** — report
   it distinctly from `cleared` and never as awaiting operator action.

3. **Report to the console** — a compact, skimmable summary, in this order:
   - One header line: date, mode (report/auto), # gates evaluated, any reachability gaps
     (e.g. "coord MCP 401 — N gates UNKNOWN").
   - **OPEN** table: `plan · id · on_open · evidence · suggested next step`.
   - **CLOSED** table: `plan · id · what it's still waiting on` (one line each).
   - **UNKNOWN/MANUAL** table: `plan · id · why unknown / what human action it needs`.
   - A short **"Needs your attention"** list: machine-confirmed OPEN gates + notify-class
     gates awaiting the operator (credentials/billing/strategy/on-page verification).

4. **Act only if `auto`** (step covered by the Arguments section). In `report` mode, do
   **not** edit any plan file — the console report is the entire output.

Keep it tight: the value is a fast, trustworthy "what's unblocked right now" answer, not
a wall of text. End report mode by reminding the user they can run `/gate-sweep auto` to
act on the confirmed-open gates, or that the daily `QontinuiGateSweep` task will at 08:00.
