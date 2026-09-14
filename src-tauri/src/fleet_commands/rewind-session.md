---
description: Roll back a failed AI session by restoring the pre-edit snapshots taken when it touched each file, then respawn a worker with the failure context prepended.
---

Allowed tools: Read, Bash

# Rewind Session

Roll back a failed AI session by restoring the pre-edit snapshots taken
when it touched each file, then either spawn a fresh worker in the same
tab with the failed worker's `/summarize-session` output prepended as
failure context (default: option (c) "revert + replay-with-warning") or
leave the tab empty for manual re-prompt (`--no-replay`: option (b)).

## Arguments

- `$ARGUMENTS` — A `task_run_id` of the failed session, optionally
  followed by the flag `--no-replay`.
  - Default behaviour (no flag): revert + replay-with-warning. The
    `/summarize-session` output (which Phase 4 verdict-tags as
    `## Outcome: APPROACH FAILED — do not retry without addressing X`) is
    prepended to the new worker's prompt so it reads it as a failure-mode
    warning, not as instructions.
  - `--no-replay`: revert + leave tab empty for manual re-prompt. Used
    when the user wants to strategically reframe rather than auto-retry.

## Instructions

### 1. Validate input

If no `$ARGUMENTS` was given, abort with "rewind-session requires a
task_run_id". If the task_run_id does not match an existing session,
abort with "no session with that id".

### 2. Restore files via the runner endpoint

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

If `errors` is non-empty, print them and abort — do not proceed to kill
or replay because the workspace is in a partial state.

For verification, the endpoint reads each `session_file_snapshots.blob_path`
from disk, compares its SHA-256 to the stored `blob_sha256`, and only
copies-over the file if the digest matches. Mismatches go into the
`errors` array.

If you need a manual fallback (the convenience endpoint is down):

1. `GET /sessions/<task_run_id>/snapshots` to list the snapshot rows.
2. For each row, run `sha256sum "<snapshot_blob_path>"` and confirm the
   first column matches `blob_sha256`.
3. `cp "<snapshot_blob_path>" "<file_path>"` to restore.

### 3. Capture failure context (skip if `--no-replay`)

Run `/summarize-session <task_run_id>` first (if it has not already been
run). Capture the resulting markdown — for `verdict=needs_fix` /
`verdict=escalate` sessions Phase 4 requires every learning body to lead
with a `## Outcome: APPROACH FAILED — do not retry without addressing X`
header, so the new worker reads it as a warning.

### 4. Kill the failed session

`POST /sessions/<task_run_id>/kill` to stop the failed worker. If the
session was already terminated this is a no-op; surface any error verbatim.

### 5. Spawn the replacement (skip if `--no-replay`)

`POST /sessions/spawn` with body:

```json
{
  "role": "worker",
  "tab_id": "<the same tab_id the failed worker occupied>",
  "initial_message": "<failure-context block>"
}
```

The `<failure-context block>` is the verdict-tagged `/summarize-session`
markdown from step 3, preceded by a one-line preamble:

```
The previous attempt at this task failed. Below is the failure summary.
Read it as a warning about what NOT to retry; the file system has been
reverted to the state before that attempt.

<paste /summarize-session output here>
```

Default role is `worker` — do NOT pin a more specific role. Phase 4 §9 Q5
resolution is explicit that the user can intervene if a different role is
needed.

If `--no-replay`, skip steps 3 and 5. Print "Tab cleared; reverted N
files. Re-prompt manually." and exit.

### 6. Report

Print:

```
Rewound session <task_run_id>:
  files restored: <N>
  files skipped (no snapshot): <M>
  failed worker killed: yes
  replay spawned: yes | no
  new session id: <id, or "—" if --no-replay>
```

## Rules

- **Snapshots are advisory.** Files the failed worker touched but for
  which no snapshot was taken (rare — should only happen if the
  dispatcher snapshot path was disabled) are left untouched.
- **Verify every restore.** The endpoint refuses to write a file whose
  blob's sha256 disagrees with the stored hash; that is non-negotiable.
- **Preserve other sessions' work.** Only files registered to this
  session via `session_file_snapshots WHERE session_id = $1` are
  candidates. Files this session never touched are never modified.
- **Default to replay.** Per §9 Q5 resolution the user wanted (c) as the
  default — only honour `--no-replay` when the user explicitly typed it.

## Implementation Notes

$ARGUMENTS
