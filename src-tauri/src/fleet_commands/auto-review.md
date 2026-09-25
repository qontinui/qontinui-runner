---
description: Review a spawned session's work against the prompt it was given, run the relevant tests, and emit a verdict + confidence score — into the reviews table when a project.tasks id is supplied, otherwise inline plus a coord finding.
---

Allowed tools: Read, Grep, Glob, Bash

# Auto-Review

Review the work a session did against the prompt it was spawned with, run the
relevant tests, and emit a verdict + confidence score. The verdict is what a
human (or, once plan
`2026-09-12-consolidate-local-orchestration-onto-conductor` Phase 6 lands, the
orchestration loop) reads to decide whether the work merges, needs a fix pass,
or escalates.

You may NOT edit any source code. If you spot a bug, you describe it; you
don't fix it.

## What this command reviews AGAINST — and why there is no task row

Until 2026-09-15 this command started from a task row: `GET /tasks/<task_id>`
gave it a description, `expected_file_claims`, a plan path and a
`plan_version_hash`. Those reads went with the Productivity scheduler and
plan/task board (plan `2026-09-12-consolidate-local-orchestration-onto-conductor`
Phase 4): no `GET /tasks/<id>` or `GET /sessions/<id>/task` route exists on the
runner at all — the only `/tasks/*` routes were the completion-report ones in
`mcp/completion_reports.rs`, a module that phase deletes. So the review
operates on the SESSION'S OWN RECORD instead:

- its spawn prompt and name — `GET /task-runs/<task_run_id>` (`prompt`,
  `task_name`);
- what it actually did — `GET /sessions/<task_run_id>/transcript`;
- the files it touched — `GET /sessions/<task_run_id>/touched-files`;
- its diff — `git` in the worktree those paths resolve to.

There is no plan-version check any more, because there is no plan hash to
check against; the prompt the session was given IS the assignment.

## Arguments

- `$ARGUMENTS` — the `task_run_id` of the session under review, optionally
  followed by `--task <uuid>`: a `project.tasks` id to attach the review row
  to. `POST /reviews` REQUIRES one (`project.reviews.task_id` is `NOT NULL
  REFERENCES project.tasks(id)`), and no surviving route resolves a task id
  from a session id, so it comes from the caller or the review is not
  persisted to that table — see Step 6.

## Instructions

### 1. Resolve the session

`GET /task-runs/<task_run_id>`. A `null` body means no such session — abort
with "no session with that id". Keep `prompt` (the assignment; it is
`Option<String>` — a `null` prompt means the assignment is UNKNOWN, so review
scope from the transcript and say so, never treat null as "no scope") and
`task_name`.

If `reviewer_session_id == reviewed_session_id` (you were asked to review
yourself), abort: `POST /reviews` rejects self-review with 409, and an inline
verdict on your own work is worth nothing either.

### 2. Read the assignment and the worker's transcript

- The assignment is the `prompt` from Step 1. If it names a plan or a file,
  `Read` it and focus on the section it points at.
- `GET /sessions/<task_run_id>/transcript` — the runner returns the persisted
  `output_log` when it has one, else the on-disk Claude Code JSONL for that
  session id; either way a `messages` array.

### 3. Inspect the diff

- `GET /sessions/<task_run_id>/touched-files` → `files` (absolute paths, from
  `session_touched_files`).
- Resolve the repo: `git -C "$(dirname <first touched path>)" rev-parse
  --show-toplevel`. If the touched paths span more than one repo, review each.
- `Bash: git -C <repo> diff --stat` and `git -C <repo> diff -- <touched
  files>`. If the session committed, include `git log --oneline
  origin/main..HEAD` and diff against the merge-base instead of the working
  tree.
- Cross-reference: did the worker touch paths its prompt gives no reason to
  touch? List them. This replaces the old `expected_file_claims` check — the
  prompt is the only statement of scope you have.

### 4. Run tests

If the touched files include Rust under `qontinui-runner/src-tauri/`:
- `Bash: cd qontinui-runner && bash <workspace-root>/qontinui-claude-config/scripts/cargo-guard.sh check --all-targets` (the checkout is shared; never raw `cargo` there)
- `Bash: cd qontinui-runner && bash <workspace-root>/qontinui-claude-config/scripts/cargo-guard.sh test -- <module filter>`
  on the modules likely affected (use `--test <name>` filters when the touched
  set is narrow).

If the touched files include TypeScript:
- `Bash: cd qontinui-runner && npx tsc --noEmit`
- The relevant `vitest` invocation if test files exist alongside.

For unfamiliar test surfaces, refuse to run blind and note "tests not run:
unfamiliar surface" in the reasoning rather than fabricating a green light.

### 5. Form a verdict

Decide one of:

- **APPROVED** — diff is consistent with the prompt, scope aligns, all tests
  pass, no obvious correctness/security issues.
- **NEEDS_FIX** — concrete defects identified that the worker can address with
  guidance. List defects with file:line.
- **ESCALATE_TO_USER** — the prompt's assumption is wrong, the change has scope
  beyond it, or you can't form an opinion confidently. Don't overuse; see
  confidence guidance below.

Assign a `confidence` score on `[0, 1]`:

- 0.9–1.0: tests pass, scope matches, diff is small and obvious.
- 0.7–0.89: tests pass, scope matches, but the change is large or touches
  unfamiliar territory.
- 0.5–0.69: partial coverage (some tests didn't run, or the scope is
  ambiguous).
- < 0.5: you can't form a real opinion. Combined with `ESCALATE_TO_USER`.

Nothing acts on the score automatically today. The scheduler that once turned
`approved >= 0.85` into an auto-merge was deleted with Phase 4; until Phase 6
re-attaches the verdict to orchestration runs, a human reads it — via
`GET /sessions/<task_run_id>/latest-review`, `GET /reviews/recent`, or the
inline report below.

### 6. Persist

**With `--task <uuid>`:** `POST /reviews` with:

```json
{
  "task_id": "<the --task uuid>",
  "reviewer_session_id": "<this session's task_run_id>",
  "reviewed_session_id": "<the reviewed task_run_id>",
  "verdict": "approved | needs_fix | escalate",
  "confidence": 0.87,
  "reasoning": "<markdown body>",
  "diff_summary": {"files_changed": 4, "lines_added": 120, "lines_removed": 30},
  "test_results": {"cargo_check": "ok", "cargo_test": "passed (47 tests)"}
}
```

`task_id`, both session ids and `reasoning` must be non-empty (400
otherwise); a `task_id` with no `project.tasks` row fails the foreign key
(500). The endpoint inserts the row. It is a plain write — the `review-completed`
Tauri event it used to emit went with its only two subscribers (the board's
`ReviewBadge` and the coordinator observe loop) in Phase 4 of
`2026-09-12-consolidate-local-orchestration-onto-conductor`. Read the row
back with `GET /reviews/recent` or `GET /sessions/<id>/latest-review`.

**Without `--task`:** the row cannot be written — say so in the report rather
than inventing a task id. Record the verdict where a later session can find
it: `coord_post_finding` when the tool is visible (title
`auto-review <task_run_id>: <verdict> <confidence>`, the reasoning as the
body), else the inline report is the only copy and you say that too.

### 7. Report

One paragraph: verdict, confidence, top reason, and WHERE the full reasoning
went (reviews row, coord finding, or inline only). The conversation summary
is for the human-skimming-the-tab-while-the-agent-runs case.

## Rules

- **No edits, ever.** You are read-only on source code.
- **Don't fabricate test results.** If a test surface is unfamiliar, say so.
  Confidence drops accordingly.
- **Be specific in NEEDS_FIX.** A reasoning of "looks wrong" with no
  file:line citations is grounds for the user to drop the verdict and re-run.
- **Don't review yourself.** If `reviewer_session_id == reviewed_session_id`
  the endpoint will reject with 409. Check before you start, not at persist
  time.
- **Don't invent a task id.** An unpersisted review that says so is honest; a
  review attached to a random `project.tasks` row is a lie in a table other
  code reads.

## Implementation Notes

$ARGUMENTS
