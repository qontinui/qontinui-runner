---
description: Roll back a failed AI session by restoring the pre-edit snapshots taken when it touched each file, then respawn a fresh session with the failure context as its first prompt.
---

Allowed tools: Read, Bash

# Rewind Session

Roll back a failed AI session by restoring the pre-edit snapshots taken
when it touched each file, then either spawn a fresh session with the
failed session's `/summarize-session` output prepended as failure context
(default: "revert + replay-with-warning") or leave it at the revert for a
manual re-prompt (`--no-replay`).

## Arguments

- `$ARGUMENTS` — A `task_run_id` of the failed session, optionally
  followed by the flag `--no-replay`.
  - Default behaviour (no flag): revert + replay-with-warning. The
    `/summarize-session` output (which verdict-tags a failed session as
    `## Outcome: APPROACH FAILED — do not retry without addressing X`) is
    the new session's first prompt, so it reads it as a failure-mode
    warning, not as instructions.
  - `--no-replay`: revert only. Used when the user wants to strategically
    reframe rather than auto-retry.

## Instructions

### 1. Validate input

If no `$ARGUMENTS` was given, abort with "rewind-session requires a
task_run_id". `GET /task-runs/<task_run_id>`; a `null` body means no such
session — abort with "no session with that id". Keep the response: its
`task_name` names the replacement in Step 5.

### 2. Make sure the failed session is not still editing

The runner exposes no per-session kill over HTTP — `/sessions/<id>/kill`
never existed, and `POST /task-runs/<id>/stop` kills EVERY tracked AI
process on the box (`current_ai_pids` is runner-global), so it is not a way
to stop one session. Instead:

1. `GET /sessions/idle-status` — the idle table of the SDK-backed Claude
   sessions the `SessionManager` holds (`claude_session/manager.rs::snapshot`);
   pty-backed workers and terminal-tab sessions are not in it, so a session
   ABSENT from the table is UNKNOWN, never idle. A rewind target wrote
   `session_file_snapshots`, which only the SDK dispatcher does, so it is the
   right table here. If the failed session is listed and not idle, stop and
   tell the user to close its tab first; restoring files under a session that
   is still writing them is how a rewind gets clobbered.
2. `POST /sessions/<task_run_id>/finish` with `{"reason": "rewind-session:
   reverted and replayed"}` — metadata only, never touches the process. A
   rebuilt runner then stops offering the failed session for resume, and on a
   FIRST finish the mark is also queued to coord by the finish outbox (it
   leaves the box). **Only on a first finish.** `set_finished`
   (`src-tauri/src/session/session_lifecycle_store.rs`, verified at the
   runner's `origin/main` `4ad6d7350`) writes `rec.finish_synced = false`
   inside `if rec.finished_at.is_none()` alone; the reason-overwrite branch
   that runs below it (`if let Some(r) = non_empty(reason)`) leaves the flag
   untouched. So on an already-finished, already-SYNCED session the `200`
   this step correctly predicts rewrites `finish_reason` **locally only** and
   never re-queues it — coord keeps the earlier reason. Do not report that
   second-finish reason as having reached coord.
   `404` means the session has NO lifecycle record — `record_open` runs only
   for terminal-plane tabs and Conductor workers, so an SDK session spawned
   through `POST /sessions/spawn` never has one — and the marker simply does
   not apply; report it `n/a`. A `200` on a session that was already
   finished OVERWRITES its `finish_reason` (the store answers 404 for a
   re-finish only when no reason is supplied, and this call always supplies
   one).

### 3. Restore files via the runner endpoint

`POST /sessions/<task_run_id>/rewind` — the runner's convenience endpoint
that does the actual file restore + sha256 verification, so this slash
command body does not need to orchestrate `cp` calls inside the LLM
context. Body:

```json
{}
```

Response shape:

```json
{
  "filesRestored": <int>,
  "filesSkipped": <int>,
  "errors": [{ "filePath": "...", "reason": "..." }]
}
```

If `errors` is non-empty, print them and abort — do not proceed to replay
because the workspace is in a partial state.

For verification, the endpoint reads each `session_file_snapshots.blob_path`
from disk, compares its SHA-256 to the stored `blob_sha256`, and only
copies-over the file if the digest matches. Mismatches go into the
`errors` array.

If you need a manual fallback (the convenience endpoint is down):

1. `GET /sessions/<task_run_id>/snapshots` to list the snapshot rows.
2. For each row, run `sha256sum "<snapshot_blob_path>"` and confirm the
   first column matches `blob_sha256`.
3. `cp "<snapshot_blob_path>" "<file_path>"` to restore.

### 4. Capture failure context (skip if `--no-replay`)

Run `/summarize-session <task_run_id>` first (if it has not already been
run). Capture the resulting markdown — for `verdict=needs_fix` /
`verdict=escalate` sessions every learning body leads with a
`## Outcome: APPROACH FAILED — do not retry without addressing X`
header, so the new session reads it as a warning.

### 5. Spawn the replacement (skip if `--no-replay`)

First pick the account, as served policy `production-and-cost`
`spawn-routing-reads-account-budget` requires (Phase 4 of plan
`2026-09-03-provider-limit-kills-destroy-subagent-context-and-nothing-resumes`):
`bash <workspace-root>/qontinui-claude-config/scripts/account-budget.sh pick --model <the model the spawn will run>`.
`PICK <id> …` (exit 0) names the account to pass below. `UNKNOWN_ONLY` (exit 4)
means no account is known-healthy but none is known-exhausted either — place
the spawn in one of the listed ids and say so. `ALL_EXHAUSTED
earliest_reset=<iso>` (exit 3): do not spawn into a limit and do not narrow
the work — schedule the replay against that reset with
`scripts/rate-limit-reset.sh schedule`, passing `--hint` (the resumed agent's
only context is what you put there; shape and cap: `/vet-imp-sweep` Step 6), and
report the `gate_id`. `NO_ACCOUNTS` (exit 2) means the roster resolved to
nothing — a configuration fault to report, not a reason to spawn unrouted.

Then resolve the working directory: `GET /sessions/<task_run_id>/touched-files`
→ `files`, and `git -C "$(dirname <first path>)" rev-parse --show-toplevel`.
Without `cwd` the spawn starts in the RUNNER PROCESS's cwd, which is nowhere
the replay has work — so omit it only when no path can be resolved, and say so.

Then `POST /sessions/spawn` with body:

```json
{
  "task_name": "Replay of <task_name from Step 1>",
  "prompt": "<failure-context block>",
  "account": "<the id PICK named>",
  "cwd": "<the repo root resolved above>"
}
```

**No `role` field.** `role` selects a slash command from the runner's closed
allow-list (`role_slash_command` in `src-tauri/src/mcp/sessions.rs`) and the
handler answers 400 to any other value — `"worker"` was never on it, so the
body this command carried until 2026-09-15 was refused every time. `prompt`
is the free-form first message the handler dispatches verbatim when no role
is set (`SpawnSessionRequest.prompt`, `initial_prompt_for`), and the handler
also 400s a `prompt` combined with a `role`, so the two are never sent
together. The shape is the one the runner's own context-exhaustion watcher
posts, pinned by `the_context_handoff_watcher_payload_deserializes_verbatim`
and `spawn_request_deserializes_cwd_and_defaults_it_to_none` in that file's
tests. A `cwd` that is not an existing directory is a 400, not a fallback.

> ⚠️ **Cross-repo landing dependency — the allow-list and the command files
> are two repos, and the allow-list is the permissive half.** A role ON
> `role_slash_command`'s list whose `.md` has been deleted does **not** 400:
> the spawn SUCCEEDS and hands the new session a slash command that does not
> exist. So deleting a command file here while its role is still on the
> runner's allow-list opens a spawn-into-nothing window.
>
> **Order it with a coord dep edge at PR time rather than by hand:** the
> qontinui-claude-config PR that deletes a command file carries
> `coord:downstream-of=qontinui/qontinui-runner#<n>` — the waiting side of the
> edge — so the runner PR that drops the allow-list arm lands FIRST and the
> window never opens. Deleting a command file whose role is still on the
> allow-list, with no such edge declared, is the defect this note names.
>
> No such window is open as of 2026-09-22: `role_slash_command`
> (`src-tauri/src/mcp/sessions.rs`) maps `auto-review`, `summarize-session` and
> `implement-plan`, and all three have a command file here.

The `<failure-context block>` is the verdict-tagged `/summarize-session`
markdown from Step 4, preceded by a one-line preamble:

```
The previous attempt at this task failed. Below is the failure summary.
Read it as a warning about what NOT to retry; the file system has been
reverted to the state before that attempt.

<paste /summarize-session output here>
```

The replacement gets its own tab. `SpawnSessionRequest` carries no tab id,
so "respawn in the same tab" is not something this command can ask for; the
failed tab stays where it is, marked finished by Step 2.

If `--no-replay`, skip steps 4 and 5. Print "Reverted N files. Re-prompt
manually." and exit.

### 6. Report

Print:

```
Rewound session <task_run_id>:
  files restored: <N>
  files skipped (no snapshot): <M>
  failed session marked finished: yes | n/a (no lifecycle record)
  replay spawned: yes | no
  new session id: <taskRunId from the spawn response, or "—" if --no-replay>
```

## Rules

- **Snapshots are advisory.** Files the failed session touched but for
  which no snapshot was taken (rare — should only happen if the
  dispatcher snapshot path was disabled) are left untouched.
- **Verify every restore.** The endpoint refuses to write a file whose
  blob's sha256 disagrees with the stored hash; that is non-negotiable.
- **Preserve other sessions' work.** Only files registered to this
  session via `session_file_snapshots WHERE session_id = $1` are
  candidates. Files this session never touched are never modified.
- **Default to replay.** Only honour `--no-replay` when the user
  explicitly typed it.

## Implementation Notes

$ARGUMENTS
