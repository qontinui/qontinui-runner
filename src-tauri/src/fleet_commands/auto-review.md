---
description: Review a worker session's work against its assigned task, run the relevant tests, and emit a verdict + confidence score into the reviews table (drives auto-merge vs. user approval vs. escalation).
---

Allowed tools: Read, Grep, Glob, Bash

# Auto-Review

Review the work a worker session did against the task it was assigned, run the
relevant tests, and emit a verdict + confidence score into the `reviews`
table. The verdict drives whether the work auto-merges, queues for user
approval, or escalates.

You may NOT edit any source code. If you spot a bug, you describe it; you
don't fix it.

## Arguments

- `$ARGUMENTS` — Either:
  - A `task_run_id` (the session you're reviewing), or
  - A `task_id` (the task whose currently-assigned session to review).

If both are unambiguous, prefer `task_id` — it ties to the canonical plan task.

## Instructions

### 1. Resolve identifiers

Call `GET /tasks/<task_id>` (or `GET /sessions/<task_run_id>/task` if invoked
with a session id) to get:
- The task's description, expected_file_claims, plan_version_hash.
- The plan's markdown_path.
- The reviewed session's task_run_id.

If the task's `plan_version_hash` differs from the current hash of
`markdown_path`, abort: "plan-version mismatch — re-decompose first". Don't
review against a stale plan.

### 2. Read the task and the worker's transcript

- `Read` the plan at `markdown_path`, focus on the task's section.
- Call `GET /sessions/<reviewed_session_id>/transcript` to get the worker's
  conversation. (Existing endpoint? Verify against
  `qontinui-runner/src-tauri/src/commands/transcript.rs`. If missing, add a
  thin endpoint that returns the session's stored transcript JSON.)

### 3. Inspect the diff

Run:
- `Bash: git -C <worker repo> diff --stat`
- `Bash: git -C <worker repo> diff -- <expected_file_claims joined>` to focus
  on the claimed paths.
- Cross-reference: did the worker touch any path NOT in
  `expected_file_claims`? List those. (Use the runner's
  `GET /sessions/<reviewed_session_id>/touched-files` — already exists,
  backed by `session_touched_files`.)

### 4. Run tests

If the touched files include Rust under `qontinui-runner/src-tauri/`:
- `Bash: cd qontinui-runner/src-tauri && cargo check` (fast)
- `Bash: cd qontinui-runner/src-tauri && cargo test --quiet -- --test-threads=4`
  on the modules likely affected (use `--test <name>` filters when claims are
  narrow).

If the touched files include TypeScript:
- `Bash: cd qontinui-runner && npx tsc --noEmit`
- The relevant `vitest` invocation if test files exist alongside.

For unfamiliar test surfaces, refuse to run blind and note "tests not run:
unfamiliar surface" in the reasoning rather than fabricating a green light.

### 5. Form a verdict

Decide one of:

- **APPROVED** — diff is consistent with the task description, claims align,
  all tests pass, no obvious correctness/security issues.
- **NEEDS_FIX** — concrete defects identified that the worker can address with
  guidance. List defects with file:line.
- **ESCALATE_TO_USER** — the task assumption is wrong, the change has scope
  beyond the plan, or you can't form an opinion confidently. Don't overuse;
  see confidence guidance below.

Assign a `confidence` score on `[0, 1]`:

- 0.9–1.0: tests pass, claims match, diff is small and obvious.
- 0.7–0.89: tests pass, claims match, but the change is large or touches
  unfamiliar territory.
- 0.5–0.69: partial coverage (some tests didn't run, or the claim coverage is
  ambiguous).
- < 0.5: you can't form a real opinion. Combined with `ESCALATE_TO_USER`.

Confidence-gated downstream behaviour (handled by the Coordinator):

- `verdict=approved AND confidence >= 0.85` → auto-merge gate (Coordinator
  triggers per-session commit via the traffic-light path).
- `verdict=approved AND 0.7 <= confidence < 0.85` → user approval queue (the
  dashboard shows it; user clicks approve/reject).
- `verdict=needs_fix` → re-assign to the same worker (or a new one if the
  same worker has failed twice) with the reasoning attached as the new
  prompt. Bounded to 3 retries before auto-escalation.
- `verdict=escalate AND confidence < 0.5` → user escalation, immediate.

### 6. Persist

`POST /reviews` with:

```json
{
  "task_id": "<uuid>",
  "reviewer_session_id": "<this session's task_run_id>",
  "reviewed_session_id": "<worker session>",
  "verdict": "approved | needs_fix | escalate",
  "confidence": 0.87,
  "reasoning": "<markdown body>",
  "diff_summary": {"files_changed": 4, "lines_added": 120, "lines_removed": 30},
  "test_results": {"cargo_check": "ok", "cargo_test": "passed (47 tests)"}
}
```

The endpoint inserts the row and emits a `review-completed` Tauri event so the
dashboard updates and the Coordinator's observe loop sees it.

### 7. Report

One paragraph: verdict, confidence, top reason. The full reasoning is in the
DB; the conversation summary is for the human-skimming-the-tab-while-the-agent-runs case.

## Rules

- **No edits, ever.** You are read-only on source code.
- **Don't fabricate test results.** If a test surface is unfamiliar, say so.
  Confidence drops accordingly.
- **Be specific in NEEDS_FIX.** A reasoning of "looks wrong" with no
  file:line citations is grounds for the user to drop the verdict and re-run.
- **Don't review yourself.** If `reviewer_session_id == reviewed_session_id`
  the endpoint will reject with 409. The Coordinator should never assign
  this, but check anyway.

## Implementation Notes

$ARGUMENTS
