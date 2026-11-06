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
   reverted and replayed"}`. This is metadata only and never touches the
   process. It is the same path-addressed door `/finish-session` Part B Step 1
   uses, and its answer is read the same way: from the BODY, not the status
   line. A rebuilt runner then stops offering the failed session for resume.
   Whether the runner told coord is decided by `changed` first, then by the
   field it points at; whether coord HAS the finish is decided only by coord's
   own read below the list. Do not infer either from `session.finishSynced`
   alone on a fresh mark, because that flag is false both for a mark still in
   flight and for one that will never be sent. Only a runner build predating Phase 1 omits
   `session.finishSynced`; treat an absent `session.finishSynced` as false.
   - `changed: "marker"` with `coord.queued: true`: the mark is new and the
     coord write is queued to `coord.coordSessionId`. Report both. On the
     runner build carrying plan
     `2026-10-23-coord-records-a-session-finished-but-not-when-so-a-revival-cannot-be-told-from-a-re-finish`
     Phase 3, a second rewind of an already-finished session lands here too:
     its reason makes it a new declaration, so the mark is RESTAMPED
     (`finishedAt` moves later) and re-sent, and `session.finishSynced` is
     false until coord ACKs it.
   - `changed: "marker"` with `coord.queued: false`: the resume set is
     corrected and coord is **not** told. Quote `coord.reason`
     (`no_coord_session`, `outbox_write_failed` or `no_forwarder`) and
     say so in those words.
   - `changed: "none"` (only from a runner build predating that Phase 3):
     the session was already finished with this same reason, so nothing was
     rewritten and the old `finishedAt` stands. This call always sends the
     same fixed reason, so on such a build a second rewind of the same
     session lands here. `session.finishSynced` says only whether the runner
     holds coord's ACK for the mark (absent means no); coord's own read below
     the list decides coord's half. `coord.queued: true` means the owed
     finished MARKER was re-queued (the mark was not yet synced), and
     `coord.queued: false` with `not_requeued` means nothing was owed. With
     `session.finishSynced` false and `coord.queued: false`, the runner did
     not tell coord: quote `coord.reason`, say so, and let coord's own read
     below the list say whether coord has the mark anyway. Nothing about the
     rewind reaches coord from this answer.
   - `changed: "reason_only"` (only from a runner build predating that Phase
     3): the session was already finished with a DIFFERENT reason (by
     `/finish-session`, say), and only its `finish_reason` was rewritten; the
     old `finishedAt` stands. `coord.queued: false`
     (`not_requeued`) is not a verdict here. `session.finishSynced` says only
     whether the runner holds coord's ACK for the mark (absent means no);
     coord's own read below the list decides coord's half. The rewind reason
     never reaches coord from this door on either arm: on a mark not yet synced
     `coord.queued: true` means the finished MARKER was re-queued, not the
     reason (the runner's finished row carries no reason text). Do not report
     the rewind reason as having reached coord.
   - A `200` with **no `coord` key**: the running runner build predates plan
     `2026-09-20-the-one-finish-door-that-names-its-target-in-the-path-is-disowned-by-both-closeout-commands`
     Phase 1. Coord's half is UNKNOWN on it. That build tells coord only for
     AI-plane sessions the registrar indexed, and says nothing about which.
     Report it UNKNOWN, not synced.

   **Coord's own read decides coord's half, whatever `finishSynced` says.**
   A `true` is not proof coord recorded the finish: the runner stamps it on
   any 2xx from coord's `PATCH /sessions/:id`, and coord can answer 2xx while
   its progress write did not land, leaving the row's `session_status`
   unmoved (dossier `finish-synced-true-while-coord-unfinished`; plan
   `2026-11-05-finish-synced-is-stamped-on-a-2xx-that-did-not-move-session-status`).
   A `false` is not proof coord lacks it either: an earlier finisher such as
   `/finish-session` may have written coord directly. So after any `200` that
   carries a `coord` key, read coord's work-status for the session. The finish
   route is keyed by the harness session id, so on a `200` the id you sent IS
   that id. The read goes to coord's own host, `coord.qontinui.io` (never the
   runner's loopback port the finish POST used), with a device JWT:
   `$COORD_DEVICE_JWT` first, then `~/.qontinui/coord-device-jwt`, skipping
   one whose `exp` has passed, and staged in a file, never on argv:

   ```bash
   SID='<the id the finish POST answered 200 for>'
   # The first interpreter that actually runs (a Windows Store python3 stub does not).
   PY=python3; for c in python3 python; do "$c" -c 1 2>/dev/null && { PY=$c; break; }; done
   TOK=""
   if ! printf '%s' "$SID" | grep -Eq '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'; then
     echo "SID is not a lowercase UUID: substitute the id first"
   else
     for C in "${COORD_DEVICE_JWT:-}" "$(cat ~/.qontinui/coord-device-jwt 2>/dev/null)"; do
       [ -n "$C" ] || continue
       E=$(printf '%s' "$C" | "$PY" -c 'import base64,json,sys; p=sys.stdin.read().split(".")[1]; print(json.loads(base64.urlsafe_b64decode(p+"="*(-len(p)%4))).get("exp",0))' 2>/dev/null)
       if [ "${E:-0}" -gt "$(date +%s)" ] 2>/dev/null; then TOK="$C"; break; fi
     done
     [ -n "$TOK" ] || echo "no live device JWT: coord's half is UNKNOWN (resolve one with /coord-revive)"
   fi
   if [ -n "$TOK" ]; then
     WORK=$(mktemp -d); HDR="$WORK/hdr"; OUT="$WORK/ws.json"
     ( umask 077; printf 'Authorization: Bearer %s\n' "$TOK" >"$HDR" )
     # A native curl.exe (Git Bash) cannot open mktemp's POSIX paths.
     HDRP=$HDR; OUTP=$OUT
     if command -v cygpath >/dev/null 2>&1; then HDRP=$(cygpath -w "$HDR"); OUTP=$(cygpath -w "$OUT"); fi
     H=$(curl -sS -m 30 -H @"$HDRP" -o "$OUTP" -w '%{http_code}' \
       "https://coord.qontinui.io/coord/sessions/work-status?claude_code_session_ids=$SID")
     rm -f "$HDR"
     if [ "$H" = 200 ]; then
       # Typed reads: an absent key is UNKNOWN (exit 3, nothing on stdout), never
       # a default. Substitute the session workdir as /finish-session Step 1 does.
       . <session-workdir>/.claude/skills/coord-revive/_scripts/lib/envelope.sh
       echo "sessionBridgeColumnPresent: $(envelope_require coord_work_status sessionBridgeColumnPresent "$OUT")"
       echo "unknown: $(envelope_require coord_work_status unknown "$OUT")"
       echo "invalid: $(envelope_require coord_work_status invalid "$OUT")"
       echo "status: $(envelope_require coord_work_status "statuses.$SID" "$OUT")"
     else
       echo "work-status read failed http=$H: coord's half is UNKNOWN"
     fi
   fi
   ```

   Coord has the finish only when, on that `200`, `status.session_status` is
   `"finished"`. On the `queued: true` arms `coord.coordSessionId` names a
   row, and `status.coord_session_id` must equal it; coord hears those
   through the runner's outbox, so run the read once more after ~15 s before
   calling a not-`finished` answer final. On every `queued: false` answer
   (any `coord.reason`) the runner names no row (`coordSessionId` is null),
   so quote the `coord_session_id` work-status served instead. Any other
   `session_status` on a `200` (null included), or a different row, is
   coord's half **NOT confirmed**: quote what the read returned. A failed
   read (any status but `200`; a `401` means the credential is stale, not
   that coord is down), the id listed in `unknown` or `invalid`, or
   `sessionBridgeColumnPresent: false` is **UNKNOWN**, never "not finished";
   so is a line the recipe printed EMPTY (its `UNKNOWN: coord_work_status: ...`
   on stderr names the key coord's answer did not carry).
   This command does not write coord itself, so it reports what the read
   says and never reports a `finishSynced: true` alone as coord told.

   The finish route is keyed by the runner's lifecycle-record id, the harness
   session id. It matches `<task_run_id>` only where the two ids coincide.
   `404` means the session has NO lifecycle record. `record_open` runs only
   for terminal-plane tabs and Conductor workers, so an SDK session spawned
   through `POST /sessions/spawn` never has one. Report it `n/a` only when the
   id you sent is known to be that session's harness id. When it was a
   `task_run_id` that may differ from it, the `404` is UNKNOWN, not proof the
   session is untracked. Say so.

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
> command-corpus PR that deletes a command file carries
> `coord:downstream-of=qontinui/qontinui-runner#<n>` — the waiting side of the
> edge — so the runner PR that drops the allow-list arm lands FIRST and the
> window never opens. Deleting a command file whose role is still on the
> allow-list, with no such edge declared, is the defect this note names.
>
> Whether that window is open right now is not this paragraph's to say.
> Lint check #65 (`scripts/lint-role-command-roster.py`, CI job
> `role-command-roster`) parses `role_slash_command`
> (`src-tauri/src/mcp/sessions.rs`) at a pinned runner ref and asserts, per
> role, that a command file exists both here and in the runner's bundle — read
> its result instead of a restated point-in-time measurement.

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
  coord: finished (<coord_session_id>) | NOT confirmed (<what work-status returned>) | UNKNOWN (<why>) | UNKNOWN (no finish POST 200, so no read)
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
