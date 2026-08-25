# Create Plan

Turn a prompt — inline text, or a written-up problem/feature description held in a
file — into a new implementation plan file under `$QONTINUI_PLANS_DIR` (see
[Plan directories](#plan-directories)), in the same shape the rest of the
plan-lifecycle skills (`/vet-plan`, `/implement-plan`,
`/verify-plan-status`) already expect. This is the missing **first** stage of
that lifecycle: those three all *consume* an existing plan file; nothing
before this one *authors* it. (Note: Claude Code's built-in `/plan` is the
CLI's own plan-mode toggle, not a qontinui skill — this command is the
actual "write me a plan" entrypoint.)

The plan file is the deliverable. Research the codebase enough to ground
every claim in a real `file:line`, then write a plan that `/vet-plan` can
audit and `/implement-plan` can execute without having to rediscover
anything this step already found.

## Arguments

- `$ARGUMENTS` — one of:
  - A **path** to a prompt file (e.g. `<prompts-dir>/fix-merge-train-block-reason-ux.md`,
    absolute or relative). If the path exists, `Read` it in full — its
    content is the prompt.
  - **Inline text** — a problem description, feature request, or bug report
    typed directly as the argument. Used verbatim as the prompt.
  - **Empty.** Glob the prompts directory beside the plans directory
    (`$QONTINUI_PLANS_DIR/../prompts/*.md`), sort by mtime, and confirm the most
    recently modified candidate with the user before proceeding (list the top 3–5
    by name if the most-recent guess seems ambiguous, e.g. several touched the same
    day). If no such directory exists, ask the user for the prompt instead of
    guessing — an empty argument is not a licence to invent a topic.

## Plan directories

Plan paths resolve from two environment variables. The qontinui runner injects them
into agent sessions from its `paths.plans_dir` / `paths.plans_archive_dir` settings;
a session launched outside the runner will not have them.

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

- **`$QONTINUI_PLANS_DIR`** — the directory plans live in, and the directory this
  command writes into. **If it is unset, ask the user once where plans live, or fall
  back to `<workspace-root>/plans`** (a `plans/` directory beside the repos this
  session is working in). Never assume an absolute path from another machine, and
  never write a plan to a path you had to guess without saying so.
- **`$QONTINUI_PLANS_ARCHIVE_DIR`** — optional, normally unset. When set and different
  from `$QONTINUI_PLANS_DIR`, it holds already-archived plans. This command never
  writes there, but Step 2's duplicate check must search it — an archived plan still
  counts as an existing plan.
- **Suite directories** — a multi-plan suite lives in its own directory *beside*
  `$QONTINUI_PLANS_DIR` (`$QONTINUI_PLANS_DIR/../<plan-dir>/`).

Neither directory has to be inside a git repo; this command only writes a file, so
nothing here requires one.

## Instructions

### 1. Resolve the prompt

Determine whether `$ARGUMENTS` is a real file path (`Read` it — a failure
means it wasn't a path) or inline text, per the Arguments section above.
Hold the resolved prompt text; everything below is grounded in it.

### 2. Check for an existing plan first

Before authoring anything, `Glob` `$QONTINUI_PLANS_DIR/*.md` — plus
`$QONTINUI_PLANS_ARCHIVE_DIR/*.md` if that variable is set and different — for a
title or slug that plausibly covers the same problem (grep filenames/titles
for the prompt's key nouns). **Those directories hold shipped AND unshipped
plans alike** — a plan's
status comes from its `> **Status:` block, never from which directory it sits
in (a plan is stamped where it lives). Read the stamp before dismissing
a match as unrelated.

If a close match exists, surface it to the user and ask whether to extend
the existing plan (open it and add a phase/section) instead of authoring a
duplicate — do NOT silently create a second plan for the same problem. If
nothing close exists, proceed.

### 3. Research the codebase

Identify the repo(s) the prompt touches (file paths, symbol names, or repo
names named in the prompt; if unnamed, infer from the subsystem described).
Then, in parallel:

- `Grep`/`Glob`/`Read` to confirm every file, function, and behavior the
  prompt asserts actually exists as described — prompts (like plans) often
  contain claims that are stale or slightly wrong by the time you act on
  them.
- Spawn `Explore` agents for anything broader than a targeted lookup
  (unfamiliar subsystem, "where does X actually happen" questions).
- Search for **prior art** — an existing helper, pattern, or abstraction
  that already covers part of what the prompt is asking for. The most
  common defect in hand-written plans is proposing new code that duplicates
  something that already exists under a different name; don't repeat that
  here.

Build a **Discovered prior art** table (`Piece | Location | Notes`) from
what you find — omit the section entirely if the prompt is truly a
from-scratch feature with nothing to discover.

### 4. Design the plan

Decompose the work into phases — each phase a coherent, independently
testable unit, ordered **most-falsifiable-first** (assumption-killing work
before the builds that depend on it), and sized so each phase is executable
by a single `/implement-plan` phase subagent (split further, or flag for a
multi-plan handoff, if a unit is too large for that).

Judge every design choice — pattern selection, abstraction boundaries,
scope, sequencing — against the same priorities `/vet-plan` audits against:
**powerful features → scalability → robustness → clean code** (engineering;
decides *what* gets built), gated by the **UX priorities**
(predictability → discoverability → no-surprise reversibility → honesty
about uncertainty) on any user-facing surface, sequenced per the
**implementation priorities** (verified throughput, early risk retirement,
autonomy with checks, momentum through re-planning). Programming effort and
backward compatibility are **not** factors — this project has no
backward-compatibility constraint.

Resolve open questions **now** wherever these priorities decide them —
don't leave a question dangling just because it takes judgment; write the
decision inline with one sentence naming the deciding priority (mirrors
`/vet-plan`'s Decision policy, applied at authoring time instead of after
the fact). Leave a question genuinely **open** only when it's a
product/scope/stakeholder call nobody but the operator can make.

### 5. Write the plan file

**Filename:** `$QONTINUI_PLANS_DIR/<YYYY-MM-DD>-<slug>.md`. Get today's
date from the shell — never guess or rely on training knowledge:

```bash
date +%F
```

`<slug>` is a kebab-case derivation of the plan title (3–6 words, matches
the existing corpus in `plans/*.md`).

**Structure** (matches the existing `plans/*.md` corpus and the lifecycle
`/vet-plan` / `/implement-plan` / `/verify-plan-status` all read):

```markdown
# Plan: <Title>

> **Status: DRAFT <YYYY-MM-DD>.** <one-line summary of what this plan does>.

> **Repo(s):** <repo1>[, <repo2>...]

## Why
<the motivating problem, pulled from the prompt + your own research —
not a copy-paste of the prompt>

## Design decision(s) — <name the tradeoff, omit section if none>
<only for genuinely non-obvious choices; use a comparison table like
existing plans do (see `plans/2026-05-24-symbol-claim-tenant-scoping.md`
§"Design decision" for the shape) and end with **Resolved.** + the
deciding priority>

## Discovered prior art (verified <YYYY-MM-DD>)
| Piece | Location | Notes |
|---|---|---|
| ... | `path/file.rs:123` | ... |

## Phases

**Phase 1 — <name>**
- Concrete steps, each citing `file:line` where it applies.
- Gate: <the repo's actual test/CI command, e.g. `cargo test -p qontinui-coord`>

**Phase 2 — <name>**
- ...

## Risks
- ...

## Open questions
- Only genuinely operator-only calls (see Step 4). If none remain, omit
  this section rather than leaving it empty.

## Related
- `[[other-plan-stem]]` / memory names this plan builds on or supersedes.
```

**Do not add a `## Gates` section.** That block (`<!-- GATE-SWEEP:BEGIN -->`)
is machine-managed by `/gate-sweep`, and the `unit_ready` coord gate itself
is registered by `/vet-plan` §5.4 only once the plan is stamped VETTED. A
freshly drafted plan has neither yet.

Use `Write` — this is a new file, not an edit.

**Then commit and push it immediately, stamped `DRAFT`.** Author the plan in a
worktree — never the primary/shared checkout — and commit + push the new file at
creation, before it is vetted. An untracked plan is invisible to coord's
`conflict_check` and to the plan registry (so `/preflight`'s duplicate-work guard
can't see it), and it is unreadable by whoever has to vet it. `DRAFT` is a free
status; the order is write → commit → vet. (If the plans directory is not a git
repo — see [Plan directories](#plan-directories) — the file on disk is the whole
ritual.)

Note what publishing the plan does **not** reliably buy you: it does not by itself
make the coord work unit's `vetted` status reachable. `vetted` is an *attested*
status whose attester must differ from the unit's recorded owner, and the
comparison is on the actor key `device:<uuid>` — which carries no session id, so
two device-JWT sessions on one machine are ONE actor and refuse each other
`403 self_attestation_forbidden` exactly as the author would be refused. A peer
holding a genuine agent JWT (`device:<d>:agent:<a>`) is a distinct key and does
qualify; on a device-JWT-only fleet there is no such peer. `/vet-plan` §5.4 handles
this by attempting `→ vetted` and falling back to the Free status
`vetted_unattested`, leaving the attestation outstanding for a different device or
the operator route. Publish for reviewability, `conflict_check` and the durable
record — just don't assume it unlocks attestation.

### 6. Report

Under 100 words:
- The plan's path and title.
- Phase count and repo(s) touched.
- Size of the Discovered-prior-art table (roughly — "6 prior-art hits").
- Any question left genuinely open for the user (Step 4).
- The natural next step: `/vet-plan <path>` (or `/pvi <path>` to also
  implement it end-to-end).

## Rules

- **Ground every claim.** No `file:line` citation goes into the plan
  without having been verified via `Grep`/`Read` this session. "Almost
  certainly exists" is not good enough — that's exactly what `/vet-plan`
  exists to catch, but a plan that needs no correction is a faster plan.
- **DRAFT only.** Never stamp `VETTED` — that status, and the coord
  work-unit + gate registration that comes with it, belongs solely to
  `/vet-plan`.
- **Don't duplicate an existing plan.** Step 2 is mandatory before writing.
- **Parallelize research.** Grep/Glob/Read concurrently; spawn `Explore` for
  anything broader than a single targeted lookup.
- **One new file.** The plan `.md` is the only thing this command writes.
