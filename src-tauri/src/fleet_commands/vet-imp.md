# Vet & Implement Plan

Run `/vet-plan` on a plan, then — once it's stamped VETTED — immediately run
`/implement-plan` on the same plan. One command for the full
**vet → implement** lifecycle in a single session, no stop in between.

This is a thin orchestrator: it does not re-implement any of the vetting or
implementation logic. It invokes the two canonical skills in order and passes
the resolved plan path through. All coord wiring (status publication, claim
pre-flight, `unit_ready` gate registration, the VETTED/IN PROGRESS/SHIPPED
stamps — applied where the plan lives) is owned by those two skills — do
not duplicate it here.

It also **labels the session** so the vet → implement run is easy to find in
the session list: a provisional plan-slug label at the start (Step 1.5), then a
final PR-numbered name at the end via the `/name` command, once implementation
has opened the PRs (Step 5).

## Arguments

- `$ARGUMENTS` — Path to the plan file (relative or absolute). Optional. If
  omitted, resolve the plan the same way `/vet-plan` does: look for the most
  recently modified `*.md` under `$QONTINUI_PLANS_DIR` (see below) and the
  working-tree root, and confirm the choice with the user before vetting.

  Any trailing flags `/implement-plan` understands (e.g. `--wait-timeout=<Nm>`)
  are forwarded verbatim to the implement step; they are ignored by the vet
  step.

## Plan directories

This command does not resolve plan directories itself — `/vet-plan` and
`/implement-plan` each document the full contract, and Step 1 hands both of them one
already-resolved **absolute** path. The only directory this command touches is the
omitted-argument fallback above:

> **The DB is authoritative for reads; this directory is an AUTHORING surface**
> *(plan `2026-08-16-plan-corpus-authority-and-run-provenance`, D2/D3 — canonical
> statement in `CLAUDE.md` -> "Plan corpus authority").* Discovery, search and
> selection resolve against `agent.work_artifacts` behind qontinui-web; the
> shipped runner scanner flows filesystem edits INTO it. So:
>
> * **`$QONTINUI_PLANS_DIR` being unset is NOT an error and NOT a dead end.** It
>   is a supported configuration — a tenant may author entirely through the web
>   UI and own no plans directory at all. Resolve the plan from the corpus
>   instead of asking the operator to invent a path.
> * **`qontinui-dev-notes` is this fleet's OPTIONAL export target**, never a
>   requirement. No tenant needs a git repo to author, vet or ship a plan.
> * **When qontinui-web is unreachable**, read the local degraded-mode cache:
>   `$QONTINUI_PLAN_CACHE_DIR` (default `C:/claude/plan-corpus-cache/`) —
>   `PLANS-CACHE.md` for the index, `bodies/<kind>__<slug>.md` for bodies.
>   Refresh with `qontinui-claude-config/scripts/render-plan-cache.ps1
>   -MaxAgeHours 0`. **Say plainly that you are reading a cache and quote its
>   Rendered stamp**, and treat a stale or absent cache as **UNKNOWN, never
>   empty** — "this render did not see it" is not "it does not exist".

- **`$QONTINUI_PLANS_DIR`** — the directory plans live in. The qontinui runner injects
  it into agent sessions from its `paths.plans_dir` setting; a session launched outside
  the runner will not have it. **If it is unset, ask the user once where plans live, or
  fall back to `<workspace-root>/plans`** (a `plans/` directory beside the repos this
  session is working in). Never assume an absolute path from another machine.
- **`$QONTINUI_PLANS_ARCHIVE_DIR`** — optional, normally unset. This command never
  resolves against it; if the plan the user means is already archived, pass its path
  explicitly as `$ARGUMENTS`.

Where a plan file finally *lives* after shipping — stamped in place by default — is
`/implement-plan` Step 6's call, not this command's. Do not move plan files here.

## Instructions

### Step 1 — Resolve the plan path once

Determine the single plan file this run operates on (from `$ARGUMENTS`, or via
the most-recently-modified fallback above, confirmed with the user). Hold this
resolved absolute path; both downstream skills receive the **same** path so the
vet stamp and the implement run can never drift onto different files.

If that file is still untracked in git, commit and push it — stamped `DRAFT`,
from a worktree, never the primary/shared checkout — before Step 2. `/vet-plan`
documents the same precondition: `VETTED` is an attested status a non-owner
session must be able to read, so vetting a file no peer can see defeats the
attestation.

### Step 1.5 — Provisionally label the session with the plan slug

At the start of the run no PRs exist yet, so label the session with the plan
slug as a provisional identifier. Step 5 replaces this with the final
PR-numbered name from `/name` once implementation has opened the PRs.

Derive a session label from the resolved plan filename — the basename with the
folders, the leading `YYYY-MM-DD-` date prefix, and the `.md` extension stripped
(e.g. `<plans-dir>\2026-06-18-coord-cancelled-ci-not-main-red.md` →
`coord-cancelled-ci-not-main-red`; the backslashes are deliberate — the snippet
below normalizes Windows separators) — and set it as this session's title.

The built-in `/rename` command can't be invoked from inside a slash command, so
write the same record `/rename` writes: a `custom-title` entry appended to the
current session's transcript JSONL. Run this once, substituting the resolved
absolute plan path for `<PLAN>`:

```bash
PLAN="<PLAN>"
plan_norm="${PLAN//\\//}"                       # normalize Windows backslashes
slug="$(basename "$plan_norm" .md)"
slug="${slug#[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]-}"   # drop YYYY-MM-DD- prefix if present

# Append the same entry /rename writes, to THIS session's transcript.
# Best-effort only — never block the vet → implement chain if it can't run.
if [ -n "$CLAUDE_CODE_SESSION_ID" ]; then
  sf="$(find "$HOME/.claude/projects" -name "$CLAUDE_CODE_SESSION_ID.jsonl" 2>/dev/null | head -1)"
  if [ -n "$sf" ]; then
    uuid="$(python -c 'import uuid;print(uuid.uuid4())' 2>/dev/null || echo "00000000-0000-4000-8000-000000000000")"
    ts="$(date -u +%Y-%m-%dT%H:%M:%S.000Z)"
    printf '{"type":"custom-title","customTitle":"%s","sessionId":"%s","uuid":"%s","timestamp":"%s"}\n' \
      "$slug" "$CLAUDE_CODE_SESSION_ID" "$uuid" "$ts" >> "$sf"
    echo "session labeled: $slug"
  fi
fi
```

This sets the **persisted** title — it shows immediately in the session list and
the resume picker. It does **not** live-refresh the title bar of the already
running session (that value lives in process memory and only the interactive
`/rename` updates it); the label takes visible effect on the next render of the
session list / on resume. If a live title-bar update is wanted this run, mention
the derived slug in the Step 5 report so the operator can paste `/rename <slug>`.

This step is best-effort: if `$CLAUDE_CODE_SESSION_ID` is unset or the transcript
file isn't found, skip silently and proceed — never let labeling block vetting.

### Step 2 — Vet the plan

Invoke `/vet-plan` via the **Skill tool**, passing the resolved plan path as the
argument:

```
Skill: vet-plan
Args: <resolved plan path>
```

Let `/vet-plan` run to completion — it audits the claims, edits the plan in
place, resolves open questions via its Decision policy, and stamps the plan
`Status: VETTED <date>` (and registers the `unit_ready` gate). Do not
short-circuit any of it.

**Collect its report; do not emit it.** `/vet-plan` Step 6 produces a complete,
standalone-looking report. Under this chain that report is an **intermediate
result**: hold it, and fold it into the single combined report at Step 5. Do NOT
render it as a finished deliverable here, and do NOT let it end the turn —
`/vet-plan` Step 6 has the matching instruction on its side.

The reason is mechanical, not stylistic. A finished-looking deliverable at the
midpoint is a **stop cue**: it reads as "the job is done", and the chain has been
observed to end right there with Steps 3-5 never reached (diagnosed 2026-07-28,
reproduced live). Nothing report-shaped may exist between the VETTED stamp and
the `Skill: implement-plan` call — removing that mid-chain terminal beat is the
point, so do not reintroduce it as a "quick summary of the vet" either.
(`/implement-plan` still emits its own report at the end of its run; the rule is
about the MIDPOINT, not a cap on output.)

**The gate `/vet-plan` registers now carries a continuation** (its §5.4 was
inverted 2026-07-28 for exactly this failure): if this chain drops after vetting,
coord dispatches a fresh visible session to implement the plan instead of leaving
it stranded. `/implement-plan` Step 0.5 retires that net when it stamps IN
PROGRESS; where it cannot (the continuation has not dispatched yet, so the cancel
route has nothing to act on), the residual case is a **visible** redundant
terminal that should stand down at `/implement-plan` Step 0.45 / Step 0.6 — not a
silent strand. This is a **backstop, not a licence to stop here**: a stalled
chain that gets rescued by coord still burns a session and delays the work.

### Step 3 — Gate: confirm the plan is actually VETTED

After `/vet-plan` returns, re-read the top of the plan file and confirm its
status block now reads `Status: VETTED`. This gate exists because the two
states that legitimately stop the lifecycle must stop it here too:

- **`/vet-plan` aborted** because the existing block was `SHIPPED` /
  `SUPERSEDED` / `OBSOLETE` (it refuses to overwrite a closed plan), or because
  it judged the plan's **overall architectural direction wrong** and surfaced
  that instead of editing. In either case do NOT proceed to implement — relay
  the vet skill's reason to the user and stop.
- **The stamp is missing** for any other reason. Do not implement an unvetted
  plan; report what `/vet-plan` actually produced and stop.

If the status block reads `VETTED`, proceed to Step 4. A defect count > 0 in the
VETTED summary is **not** a blocker — `/vet-plan` auto-fixes what it can and only
surfaces genuine product/scope calls; those are reported, not gating.

**Confirming VETTED and invoking `/implement-plan` happen in the SAME assistant
turn, with the Skill call last.** Do not confirm the gate in one turn and plan to
invoke in the next — there is no next turn; the turn ends and the chain is dead.
If you have just written the words that confirm the stamp, the very next thing
you emit is the Step 4 Skill call, not a summary and not a hand-off sentence.

### Step 4 — Implement the vetted plan

Invoke `/implement-plan` via the **Skill tool**, passing the same resolved plan
path (plus any forwarded implement-only flags from `$ARGUMENTS`):

```
Skill: implement-plan
Args: <resolved plan path> [forwarded flags]
```

`/implement-plan` takes over from VETTED: it runs its own dependency gate,
concurrent-work reconnaissance, claim pre-flight, the IN PROGRESS stamp, the
phase agents, manual testing, commit, and the SHIPPED stamp. Because
Step 2 just stamped the plan VETTED in this same session, `/implement-plan`'s
Step 0.5 will see a fresh VETTED block and start cleanly — it will NOT warn that
the plan was never vetted.

### Step 5 — Final session name + report

First, derive the **final session name** by invoking the `/name` command via the
**Skill tool** (no arguments — let it auto-detect):

```
Skill: name
```

`/name` detects the open PRs this run just opened and returns a name of the form
`<pr-numbers> <descriptive words>` (e.g. `614,615 coord gate robustness`),
emitted as a ready-to-run `/rename <name>` line. This supersedes the provisional
plan-slug label from Step 1.5 — now that implementation has opened PRs, the
PR-numbered name is the better identifier.

Persist that name as the session title using the **same transcript-append
mechanic as Step 1.5** (substitute the name `/name` produced for `$slug` in that
bash block). Same best-effort rules apply: if `$CLAUDE_CODE_SESSION_ID` or the
transcript file is missing, skip silently. If `/name` finds zero open PRs (e.g.
the run deferred to a gate without opening a PR), fall back to the Step 1.5 plan
slug.

Then give one short summary tying the two halves together: what the plan was
about, the vet outcome (defects found / auto-fixed / surfaced), and the
implement outcome (phases shipped, commit SHAs, anything deferred to a gate).
**Fold in the vet report you collected at Step 2** — it was never emitted, so
this is its only appearance; dropping it loses the vet outcome entirely. For the
implement half, defer to `/implement-plan`'s own report rather than repeating it
verbatim.

Surface the `/rename <name>` line from `/name` in the report so the operator can
update the live title bar of the current session if they want it (persisting the
title does not live-refresh the running session's title bar).

## Rules

- **Thin orchestrator only.** Never re-implement vetting or implementation logic
  inline. Call the two skills; let each own its coord wiring and stamps.
- **Same path through both halves.** Resolve the plan path once in Step 1 and
  pass that identical path to both skills. The Step 1.5 session label is derived
  from that same resolved path.
- **Labeling is best-effort, never gating.** Neither the Step 1.5 provisional
  label nor the Step 5 `/name` final label may block, slow, or abort the vet →
  implement chain. If the session id or transcript file is missing, skip
  silently; if `/name` finds no PRs, keep the Step 1.5 slug.
- **Never narrate the hand-off.** Do not write "proceeding to
  `/implement-plan`", "now implementing", "moving on to implementation", or any
  equivalent as the last text of a turn. Narration is not action, and this
  substitution is the single most common way this command fails: the agent
  stamps VETTED, writes the sentence, and ends the turn without ever calling the
  Skill tool (diagnosed 2026-07-28, reproduced live in the diagnosing session).
  **The Step 3 VETTED confirmation and the Step 4 `Skill: implement-plan` call
  MUST occur in the SAME assistant turn, with the Skill call LAST.** If you find
  yourself about to write that sentence — **call the tool instead.** The tool
  call IS the sentence.
- **No finished-looking report before implementation completes.** `/vet-plan`'s
  Step 6 report is collected as an intermediate result (Step 2), never emitted
  mid-chain — a finished-looking deliverable at the midpoint is a stop cue. This
  is a rule about the MIDPOINT, not a cap on output: `/implement-plan` still
  emits its own report at the end of its run, and Step 5 adds the combined
  summary on top. What must not exist is a report between the VETTED stamp and
  the `Skill: implement-plan` call.
- **The VETTED gate is mandatory.** Never run `/implement-plan` from this
  command unless Step 3 confirms a fresh `Status: VETTED` block. A vet abort
  (closed plan, or wrong architectural direction) stops the chain.
- **One session, no stop between halves.** Like `/implement-plan` itself, the
  vet → implement chain runs end-to-end without handing back to the operator
  between the two — except for the escalations the underlying skills already
  define (operator-resource needs, oversize-plan handoff, a vet abort).

## Backstop

A `Stop` hook (`scripts/vet-imp-continuation-guard.sh`, registered in
`.claude/settings.json`) catches a chain that stalls anyway. On each stop it
checks whether this session invoked `/vet-imp` and whether the plan it operated
on is still stamped `Status: VETTED` — i.e. `/implement-plan` never reached its
Step 0.5 IN PROGRESS stamp — and if so returns `{"decision":"block"}` telling the
agent to invoke `Skill: implement-plan` now.

It fires **at most once per session** (latched by
`~/.qontinui/vet-imp-guard/<session-id>`, created before the block is emitted)
and **fails open on everything else**, so it can nag but never trap. Treat it as
the last line of defence: the rules above are what should prevent the stall, and
a hook block means they were ignored. If the block is genuinely wrong (the vet
aborted, or the operator stopped the run), state that reason in one line and
stop — it will not ask twice.
