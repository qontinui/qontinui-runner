# Plan, Vet & Implement

Run the full **create → vet → implement** lifecycle on a prompt in one
command: `/create-plan` writes a new plan from the prompt, then `/vet-imp`
(`/vet-plan` → `/implement-plan`) takes it the rest of the way to shipped
code. One command, no stop in between.

This is a thin orchestrator, same discipline as `/vet-imp` itself: it does
not re-implement any plan-writing, vetting, or implementation logic. The one
thing it adds on top of `/vet-imp` is **delegating the plan-writing step to
a subagent**, specifically so the research `/create-plan` does (reading
prompt files, grepping the codebase, spawning `Explore` surveys) happens in
a disposable context instead of consuming the main session's — the main
session only needs the resulting plan *path*, never the research that
produced it.

## Arguments

- `$ARGUMENTS` — same shape `/create-plan` accepts: a path to a prompt file
  (e.g. `<prompts-dir>/fix-merge-train-block-reason-ux.md`),
  or inline problem/feature text. Forwarded to the plan-writing subagent
  verbatim.

  Any trailing flags `/implement-plan` understands (e.g.
  `--wait-timeout=<Nm>`) are forwarded through to `/vet-imp`'s implement
  step — same contract `/vet-imp` itself documents.

## Plan directories

This command does not resolve plan directories itself — `/create-plan` owns that,
and Step 3 hands `/vet-imp` an already-resolved **absolute** path. The one place
this command touches a directory is Step 2's fallback `Glob`, which uses the same
variable `/create-plan` writes into:

> **The DB is authoritative for reads; this directory is an AUTHORING surface**
> *(plan `2026-08-16-plan-corpus-authority-and-run-provenance`, D2/D3 — canonical
> statement in `CLAUDE.md` -> "Plan corpus authority").* Discovery, search and
> selection resolve against `agent.work_artifacts` behind qontinui-web; the
> shipped runner scanner flows filesystem edits INTO it (the half that writes
> *this* layer is opt-in — see the population caveat below). So:
>
> * **`$QONTINUI_PLANS_DIR` being unset is NOT an error and NOT a dead end.** It
>   is a supported configuration — a tenant may author entirely through the web
>   UI and own no plans directory at all. Resolve the plan from the corpus
>   instead of asking the operator to invent a path.
> * **`qontinui-dev-notes` is this fleet's OPTIONAL export target**, never a
>   requirement. No tenant needs a git repo to author, vet or ship a plan.
> * **A corpus that ANSWERS is not a corpus that is POPULATED.** The scanner
>   flows filesystem edits into the operational layer (`coord.work_units`)
>   whenever a plans dir and a coord base resolve, but the **body sync** that
>   fills the document layer (`body_push.rs` -> `agent.work_artifacts`) is
>   **opt-in** — built only under `QONTINUI_PLAN_LIBRARY_SYNC=1`, and gated
>   again per cycle on the tenant's `plan_capture` dial. **Either missing is a
>   silent no-op**, so a `200` carrying an empty list is **UNKNOWN, not "no
>   such plan"**: treat any zero-result corpus read as UNKNOWN unless you have
>   positively confirmed the body sync is on for this device.
> * **Do not probe by stem with `q`.** `GET /api/v1/plan-library?q=` matches
>   **title and body, NOT the slug**, so a by-stem `q` probe returns a false
>   negative for a plan that IS present. The exact door is
>   `?kind=plan&work_unit_slug=<stem>`; failing that, page `?kind=plan&limit=200`
>   and match `slug` yourself.
> * **When qontinui-web is unreachable**, read the local degraded-mode cache:
>   `$QONTINUI_PLAN_CACHE_DIR` (default `C:/claude/plan-corpus-cache/`) —
>   `PLANS-CACHE.md` for the index, `bodies/<kind>__<slug>.md` for bodies.
>   Refresh with `qontinui-claude-config/scripts/render-plan-cache.ps1
>   -MaxAgeHours 0`. **Say plainly that you are reading a cache and quote its
>   Rendered stamp**, and treat a stale or absent cache as **UNKNOWN, never
>   empty** — "this render did not see it" is not "it does not exist".

- **`$QONTINUI_PLANS_DIR`** — the directory plans live in. The qontinui runner
  injects it into agent sessions from its `paths.plans_dir` setting; a session
  launched outside the runner will not have it. **If it is unset, ask the user once
  where plans live, or fall back to `<workspace-root>/plans`** (a `plans/` directory
  beside the repos this session is working in) — and use the *same* answer the Step 1
  subagent used, never a different guess.

Step 2's fallback is a recovery path, not the normal one: prefer the absolute path
the subagent reports.

## Instructions

### Step 0 — Preflight: is this prompt already resolved?

**Run this before spawning anything.** It is two file reads and it prevents the
most expensive failure this command has: writing a fresh plan for work that
already shipped.

A `prompts/` file is an **inbox item with no lifecycle of its own — nothing
expires it**. Once its work ships it keeps reading like live work forever.
Measured cost when skipped: on 2026-08-19 a `/pvi` run on
`2026-08-04-sccache-daemon-wedge-and-s3-regression.md` spent a full cycle
re-deriving a plan root-caused 13 days earlier whose fix was already installed.

When the argument resolves to a prompt **file**, check both signals:

1. **A resolution stamp in the prompt itself** — a `> **RESOLVED …**` or
   `> **SHIPPED …**` block under the H1 (convention:
   `qontinui-dev-notes/prompts/README.md`). If present, the prompt says it is
   done; trust it and go to the disposition below.
2. **A same-stem plan** — `<plans-dir>/<same basename>.md`. That match is
   **definitive**: it is the prompt's own resolution, not prior art. Read its
   live status stamp.

```bash
stem=$(basename "<the resolved prompt path>" .md)
grep -m1 -E '^> \*\*(RESOLVED|SHIPPED|SUPERSEDED|CLOSED)' "<prompt path>"
ls "$PLANS/$stem.md" 2>/dev/null && grep -m1 -E 'Status:' "$PLANS/$stem.md"
```

A prompt that merely **cites** a plan in its prose is *not* resolved — prompts
legitimately cite prior art. Only a stamp or a same-stem plan settles it.

#### Disposition when the preflight hits

**Do not write a duplicate plan, and do not stop to ask.** Served policy
`planning-and-scope` `closeout-bookkeeping` is explicit that remaining
bookkeeping closeout "is owed its closeout: execute it fully and report
artifacts after," and that "a preceding question turn does not convert the
closeout into a fresh proposal." `finish-to-zero` adds that choosing among
discovered follow-ups "is NOT an escalation." So:

1. **Re-verify the plan's open residuals against `origin/main`, never against
   the working tree.** A local checkout that is merely an *ancestor of*
   `origin/main` shows deleted files still present and tracked, with nothing
   looking stale — that manufactures phantom open work. `git ls-tree origin/main
   <path>` is the check; `ls` is not. (This exact trap fired on 2026-08-19.)
2. **Close whatever is genuinely still open**, under the plan's own scope, with
   the normal gate (commit → PR → CI → coord merge train).
3. **Update the plan's status stamp and residual list** so the next reader is
   not sent down the same path, and **stamp the prompt** per the README
   convention.
4. Report what was already done vs. what this session closed.

Escalate only on the closed list (`escalation-bar`) — a duplicate prompt is not
on it.

If the preflight finds nothing, fall through to Step 1 unchanged.

### Step 1 — Write the plan via a subagent

Spawn one agent via the **Agent tool** (`run_in_background: false` — Step 3
cannot resolve a plan path without this agent's result, so there is nothing
useful to do in parallel while it runs). Use the default general-purpose
agent type — it needs `Write` access to create the plan file, which the
dedicated `Plan` agent type does not have.

Prompt it with something self-contained along these lines:

```
Run the create-plan skill on the following prompt, then report back only a
short summary — do NOT return the plan's contents.

Invoke via the Skill tool: skill "create-plan", args "<the resolved
$ARGUMENTS prompt text or path, verbatim>".

Let it do its own research and write the plan file. When it's done, reply
with ONLY: the plan's absolute file path, its title, phase count, and the
repo(s) it touches. Do not include the discovered-prior-art table, the
phase details, or any other plan content in your reply — the caller only
needs the path.
```

### Step 2 — Extract the plan path

Parse the subagent's reply for the plan's absolute path. If it's missing or
ambiguous, `Glob` `$QONTINUI_PLANS_DIR/*.md` (see [Plan directories](#plan-directories))
sorted by mtime and take the most recently modified file (it should be the one Step 1
just created — confirm its title matches what the subagent reported before trusting
it). Resolve that glob to a concrete absolute path before Step 3 — `/vet-imp` and the
skills below it must receive a real path, never an unexpanded variable.

Then confirm the file is committed and pushed. `/create-plan` authors the plan in
a worktree (never the primary/shared checkout) and commits it at creation stamped
`DRAFT`, because `/vet-plan` cannot attest a plan no peer session can read; if the
Step 1 subagent left it untracked, commit and push it yourself before Step 3.

### Step 3 — Vet + implement

Invoke `/vet-imp` via the **Skill tool**, passing the resolved plan path
from Step 2 (plus any forwarded implement-only flags from `$ARGUMENTS`):

```
Skill: vet-imp
Args: <resolved plan path> [forwarded flags]
```

`/vet-imp` owns everything from here: vetting, the VETTED gate, phase
implementation, testing, commit, PR, and the SHIPPED stamp. Let it run to
completion per its own rules — do not short-circuit or duplicate any of it.

### Step 4 — Report

Combine, briefly (under 100 words plus whatever `/vet-imp` itself reports):
- The plan Step 1 produced (path, title, phases, repos) — one line.
- `/vet-imp`'s own end-of-run summary (vet defects found/fixed, implement
  outcome, PR/commit info, the `/rename` line it surfaces) — don't repeat it
  verbatim, just make sure it reaches the user.

## Rules

- **The plan-writing subagent must write the file itself.** The entire
  point of Step 1 is keeping `/create-plan`'s research out of the main
  session's context — never have it return the plan text so the main
  session can write the file; that defeats the purpose.
- **Thin orchestrator only.** Never re-implement `/create-plan`, `/vet-plan`,
  or `/implement-plan` logic inline — call the skills, let each own its
  behavior and coord wiring.
- **Foreground the plan-writing agent.** Step 3 depends on its result; there
  is no independent work to overlap it with, so spawn it synchronously.
- **One session, no stop between stages** — same as `/vet-imp` — except for
  the escalations `/create-plan` (Step 2 duplicate-plan check) and
  `/vet-imp` (its own documented escalations) already define.
