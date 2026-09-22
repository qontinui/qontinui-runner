---
description: Read a finished AI session's transcript and emit "what I learned about <area>" notes into the productivity_knowledge table, indexed for cross-session discovery.
---

Allowed tools: Read, Bash

# Summarise Session

Read a finished AI session's transcript and emit "what I learned about <area>"
notes into the `productivity_knowledge` table, indexed for cross-session
discovery.

## Arguments

- `$ARGUMENTS` — A `task_run_id` of a completed session.

## Instructions

### 1. Load the transcript

`GET /sessions/<task_run_id>/transcript`. If the session is still running,
abort: "session is not yet complete; summarise after `done` state".

Also fetch the session's verdict: `GET /sessions/<task_run_id>/latest-review`
→ `review.verdict` (`review` is `null` when no review exists). This drives the
Outcome tag in step 5. A `null` review is **UNKNOWN — never `approved`**: no
review having run is not a review that passed, and resolving that absence to
the permissive value is how an unreviewed failure gets summarised as a normal
session. Carry the verdict as `unknown`, emit no Outcome tag, and SAY in the
summary that no review existed — do not emit a failure summary either, which
would be the same mistake in the other direction.

> **The runner carries TWO copies of the old rule, and only one of them ends
> by deletion. Both are verified at the runner's `origin/main` `4ad6d7350`,
> 2026-09-16.**
>
> - **`src-tauri/src/productivity/summarize.rs` — deleted, not corrected.**
>   It maps `Ok(None) => "approved"`. **Do not open a follow-up for this
>   half.** Phase 4 of
>   `2026-09-12-consolidate-local-orchestration-onto-conductor` removes
>   `src-tauri/src/productivity/` entirely, `summarize.rs` with it, along with
>   the `summarize_session` Tauri command; this command file drives the runner
>   over HTTP routes that survive (`POST /productivity-knowledge`,
>   `GET /sessions/<id>/latest-review`), so it is unaffected.
> - **`src-tauri/src/fleet_commands/summarize-session.md` — a second
>   canonical copy, untouched by Phase 4's first four commits and corrected
>   in its final one.** Its lines 26-27 still read *"If
>   no review exists yet, treat the verdict as `approved` — i.e. emit a normal
>   summary, not a failure summary."* That file is not a staged copy of this
>   one: `src-tauri/src/fleet_commands.rs` says in its module header that the
>   `fleet_commands/*.md` files are the CANONICAL fleet-shipped sources
>   (`summarize-session` is registered in `FLEET_COMMANDS`), and
>   `provision_fleet_commands_into` writes each one into a spawned session's
>   working directory unless the destination already exists AND is tracked in
>   the enclosing repo (`provision_guard.rs`: `dst.exists() && contains(rel)`)
>   — false in an allocated worktree of any repo EXCEPT qontinui-claude-config,
>   which tracks its own copy at `.claude/commands/`, so there the tracked file
>   is left in place and is what the session reads. That exception is the point:
>   this copy wins exactly where it is tracked, and the runner's wins everywhere
>   else, and the
>   failure this rule exists to prevent — an unreviewed failed session
>   summarised as if it were a normal one — still happens in the majority
>   case.
>
> **Status: the runner-side correction is CARRIED, not owed.** The Phase 4
> qontinui-runner branch
> `agent/eb2155ed4152-01a0a7d44995/2026-09-12-consolidate-local-orchestration-onto-conductor-ph`
> rewrites those lines to the UNKNOWN rule above, byte-identical to the step-1
> and step-5 text in this file (both copies diffed mechanically 2026-09-16).
> **No follow-up is owed — do not open one.** The divergence is live on the
> runner's `origin/main` (`4ad6d7350`, still blob `4b7004b7a`, still
> `approved`) only until that PR merges, and this PR carries
> `coord:downstream-of=qontinui/qontinui-runner#<n>`, which puts this repo on
> the waiting side of that edge — so the two copies land in the safe order and
> a spawned session never sees the corrected config copy beside an
> uncorrected runner one.

### 2. Identify learnings

A "learning" is a non-obvious fact about the codebase the worker discovered
during the session. Examples that ARE learnings:

- "`FileRegistryManager.normalize_path` lowercases drive letters on Windows."
- "The dispatcher emits `file-lock-acquired` only after a wait, not on
  immediate acquisition."
- "PG migrations live inline in `mod.rs::MIGRATIONS`, not in a separate
  `migrations/` directory."

Examples that are NOT learnings:

- "I edited file X."
- "The test passed."
- "The task was done."

Aim for 1–4 learnings per session. Many sessions have zero — don't manufacture.

This command runs after EVERY task completion, not just successful ones.
Failed-attempt summaries are the highest-leverage knowledge — they prevent
the next session from rediscovering the same dead end. So if the verdict was
`needs_fix` or `escalate`, still emit learnings; the failure mode itself is
often the most valuable learning.

### 3. Bucket by area

Each learning has an `area`. Pick one of: `executor`, `claude_session`,
`dispatcher`, `database`, `ui-bridge`, `frontend`, `migrations`, `tests`,
`orchestration`, `testing-infra`, or `other`. Be willing to invent new areas
sparingly — they're free-form text and grouped via FTS.

### 4. Tag failed-attempt outcomes (REQUIRED for verdict=needs_fix /
verdict=escalate)

If the session's verdict is `needs_fix` or `escalate`, EVERY learning's
`body` MUST lead with a single H2 outcome line that flags the failure mode
explicitly so future sessions read the row as "this approach failed", not as
instructions to follow:

```markdown
## Outcome: APPROACH FAILED — do not retry without addressing X

<rest of the learning body, with file:line citations>
```

Replace `X` with the concrete blocker the next session must resolve before
re-trying (e.g. "the migration ordering causes a TIMESTAMPTZ drift" or
"the snapshot prune races with the dispatcher snapshot insert"). The tag
line is parsed by the knowledge browser to surface failure-mode warnings
distinctly.

For `verdict=approved` sessions, omit the Outcome tag — the body is a
positive learning and standing in for it would be misleading. For
`verdict=unknown` (no review existed — step 1) omit it too, but for the
opposite reason: there is no verdict to tag either way. Say so in the body —
"no review had run on this session" — so the row is not read as an approval.

### 5. Persist

For each learning, `POST /productivity-knowledge` with:

```json
{
  "task_id": "<uuid or null>",
  "session_id": "<task_run_id>",
  "area": "executor",
  "summary": "1-3 sentence summary",
  "body": "full markdown explanation with file:line citations",
  "embedding_b64": "<optional base64 of 1536 bytes>"
}
```

The endpoint embeds the `body` server-side via the existing pipeline at
`qontinui-runner/src-tauri/src/rag/embeddings.rs` when the request omits
`embedding_b64`, the optional field declared in
`qontinui-runner/src-tauri/src/mcp/knowledge.rs` — `embedding_b64` is that
endpoint's field, not a symbol in the embedding pipeline. You only need to
compute the embedding yourself if you want to override the default; otherwise
leave the field absent.

### 6. Report

Print the count of learnings emitted by area. The user can browse them via
the knowledge browser (Ctrl+Shift+K).

## Rules

- **Cite the code.** Every learning's body should include at least one
  file:line reference.
- **Don't summarise the session itself.** A summary of "the worker did X then
  Y then Z" is not a learning. The transcript is the summary; the knowledge
  table is for facts that survive the session.
- **Pull learnings from observation, not from speculation.** "I noticed X"
  beats "I think X".
- **Always tag failed-attempt outcomes.** A `verdict=needs_fix` or
  `verdict=escalate` summary that doesn't lead with the Outcome tag is a
  poison pill — the next session will read it as guidance.

## Implementation Notes

$ARGUMENTS
