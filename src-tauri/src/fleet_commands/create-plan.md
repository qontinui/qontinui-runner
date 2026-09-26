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

Plan paths resolve from one environment variable. The qontinui runner injects it
into agent sessions from its `paths.plans_dir` setting; a session launched outside
the runner will not have it.

<!-- plan-corpus:start -->
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
> * **`qontinui-dev-notes` is an OPTIONAL export target as a product matter**,
>   never a requirement — no tenant needs a git repo to author, vet or ship a
>   plan. Which directory THIS fleet writes new plans to is a local operating
>   rule (`CLAUDE.md` -> "Plan corpus authority"), not a product one.
> * **Read the corpus through these doors, in this order** *(plan
>   `2026-08-27-plan-corpus-read-path-is-dark` Phase 4)*:
>   1. **The runner door — no credential.**
>      `GET http://127.0.0.1:9876/plan-library/search?kind=plan&slug=<stem>`,
>      then `GET http://127.0.0.1:9876/plan-library/artifacts/<id>` for the body
>      on a runner build that carries it. The runner attaches its own device
>      JWT; the caller presents nothing. A non-2xx names the host the runner
>      dialled — that is the runner's configured web base, and the answer is an
>      observation about that base, never about the corpus.
>   2. **The git doors — no credential, no service.**
>      `git -C qontinui-dev-notes show origin/main:plans/<stem>.md` for a body,
>      `git -C qontinui-dev-notes ls-tree --name-only origin/main plans/` to
>      enumerate. Authoring layer only (a plan authored through the web UI is
>      invisible here), exact stem match, `origin/main` as of the last fetch —
>      so fetch first:
>      `git -C qontinui-dev-notes fetch origin +refs/heads/main:refs/remotes/origin/main`,
>      exit code read unpiped (a bare `fetch origin main` in a clone whose
>      refspec does not cover `main` exits 0 and moves only `FETCH_HEAD`).
>      When that fetch was skipped or exited non-zero, a git-door MISS is
>      UNKNOWN (the ref may predate the plan); a hit still shows the plan
>      reached `origin/main`, but its body may lag it.
>   3. **The deployed door — a coord DEVICE JWT.**
>      `https://api.qontinui.io/api/v1/plan-library?kind=plan&slug=<stem>`, bearer
>      staged off argv. `~/.qontinui/coord-device-jwt` carries the `user_id`
>      claim the route requires; the agent token `/agents/allocate` mints does
>      not. `http://127.0.0.1:8000` is a per-box dev backend, not a discovery
>      door; whatever it answers is an observation about that process.
>
>   On every list result **check that the returned `slug` equals the stem** — a
>   backend predating the `slug` filter ignores the parameter and returns an
>   unfiltered page (`work_unit_slug=<stem>` is the older exact door; it is null
>   for a hand-`POST`ed row) — and **read `corpus_health`**: a `plan_count` far
>   below the `ls-tree` count is a FROZEN corpus, and the sentence to write is
>   that observation. An `ls-tree` count taken after a skipped or failed fetch
>   describes an older `origin/main`, not the current one: it can hide a frozen
>   corpus or suggest one that is not there, so record it as UNKNOWN.
>   Never `q=<stem>`: it matches title and body, not the slug.
> * **A zero is UNKNOWN until a count says otherwise.** The body sync that fills
>   `agent.work_artifacts` is a property of each writing device's runner build
>   (opt-in under `QONTINUI_PLAN_LIBRARY_SYNC=1` before plan
>   `2026-09-03-plan-library-write-door-nonce-authorized-and-body-sync-on-by-default`
>   Phase 3, on by default after it) and is gated per cycle on the tenant's
>   `plan_capture` dial, so a `200` carrying an empty list from a frozen corpus
>   is byte-identical to "no such plan". Record `corpus_health` (or the two
>   counts, with the fetch's exit status beside the `ls-tree` one) beside the
>   zero, and never write a cause for a door that did not
>   answer — settle one against a second independent instance first.
> * **The scan-root roll-up can make a miss UNKNOWN; it never proves a plan
>   absent.** `corpus_health.scan_roots.by_source_repo` (under `data` on the
>   runner door) has one roll-up per `source_repo` key: the
>   `<repo>/<dir relative to the repo root>` of a device's `paths.plans_dir`,
>   so a hit on `origin/main:plans/<stem>.md` in a checkout named `<repo>` has
>   the key `<repo>/plans`.
>   - **No git door found the file:** the roll-up has nothing to add; the miss
>     is UNKNOWN on its own unless it came after the door-2 fetch exited 0,
>     and even then it speaks for the authoring layer only.
>   - **A git door found it, and you read by `slug=<stem>`:** the miss is
>     UNKNOWN unless both hold: (a) the roll-up for the file's key reads
>     `state: measured` with `min_behind: 0` (its `min_behind_is_floor` is
>     then always `false`); (b) after a `git fetch`,
>     `git -C qontinui-dev-notes cat-file -e <ref_sha>:plans/<stem>.md` exits
>     0 (any other exit, including an object this clone lacks, is UNKNOWN).
>     Read `ref_sha` off any `scan_roots.rows[]` entry whose `device_id` is in
>     `least_behind_device_ids` (they share it); that row's own `state` may
>     read `unknown` with a `ref_stale:` detail, and the roll-up's verdict is
>     the one that counts. Everything else is UNKNOWN: no `corpus_health` or
>     no `scan_roots` (on `/candidates`, `corpus_health_unavailable_reason`
>     names why), `scan_roots.state: unknown` (`no_observation:` or
>     `read_failed:`), no roll-up for the key, an `unknown` roll-up,
>     `min_behind` above 0, or `cat-file` failing.
>   - **Any other read** — `/candidates`, or a `status`, `q`,
>     `work_unit_slug`, `repo`, `intent_ref` or `since` filter — stays
>     UNKNOWN whatever the roll-up says: any writer can replace a row's body
>     or its metadata (`status`, `work_unit_slug`, `repos`, ...) without the
>     file changing, and nothing puts the file's values back until a scanner
>     re-sends it (a runner start or scan-loop restart, a change to its plans,
>     archive or prompts dir, or a `qontinui-pr plan-library-backfill` run).
>   - **Even when (a) and (b) hold,** the roll-up only stops adding doubt; the
>     miss is no stronger than the doors that produced it. The reading can be
>     up to ~45 min old when you read it (plus one reconcile tick), and the
>     ref it counted against can have been fetched up to 6 h before that
>     reading. It establishes neither that the working tree the body sync
>     scans still held the plan (a commit of its own or uncommitted work can
>     remove it) nor that the sync wrote it under `kind=plan`: a push not yet
>     made; a paused or failing sync; a runner that stopped, or a capture dial
>     shut, within the last 45 min; a file its scan skips (no line whose first
>     non-blank character is `#` and no `> **Status:` stamp, or unreadable,
>     including not UTF-8); a kind fork, which writes nothing; or a row whose
>     kind another write moved away from `plan`, locked or not (retry with
>     `slug=<stem>` and no `kind`).
>   - **Never measured:** `min_behind` counts default-branch commits, never
>     missing plans. The archive and prompts scan roots have no reading, and
>     neither does a writer that posts none: e.g. the web UI, a hand `POST`,
>     the runner's write door, `qontinui-pr plan-library-backfill`, a
>     secondary or temp runner instance, a runner build predating the report.
> * **The cache is one line.** `scripts/render-plan-cache.ps1` needs a
>   PowerShell interpreter (`pwsh` on Linux via
>   `scripts/install-pwsh-linux.sh`); where none is present it is INOPERATIVE,
>   not a degraded arm. When you read `$QONTINUI_PLAN_CACHE_DIR/PLANS-CACHE.md`,
>   say so and quote its `Rendered:` stamp with the `api_base` beside it and
>   its `Last attempt:` line; stale or absent is UNKNOWN, never empty.
<!-- plan-corpus:end -->

- **`$QONTINUI_PLANS_DIR`** — the directory plans live in, and the directory this
  command writes into. **If it is unset, ask the user once where plans live, or
  DISCOVER one: from the workspace root, `ls -d plans */plans 2>/dev/null` and use
  the directory that actually exists** — say which, and ask when it finds none or
  more than one. Never fall back to a directory you have not confirmed is there; a
  named fallback fails silently on every machine that does not have it. Never assume
  an absolute path from another machine, and never write a plan to a path you had to
  guess without saying so.
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

Before authoring anything, check **both** layers. They fail in different
directions, which is the whole reason to ask twice.

**a. The corpus** — the authoritative surface for discovery per the block above.
Where you already have a candidate stem, use the exact door; there is no `slug`
filter, so `q` is not the fallback:

```
GET <web-origin>/api/v1/plan-library?kind=plan&work_unit_slug=<stem>
```

Otherwise page `?kind=plan&limit=200` and match `slug` and title yourself.
**Never probe by stem with `?q=`** — it matches title and body, not the slug, so
it returns a false negative for a plan that is present.

**b. The plan directory**, when `$QONTINUI_PLANS_DIR` is set — `Glob`
`$QONTINUI_PLANS_DIR/*.md` for a title or slug that plausibly covers the same
problem (grep filenames/titles for the prompt's key nouns). **That directory
holds shipped AND unshipped plans alike** — a plan's status comes from its
`> **Status:` block, never from which directory it sits in (a plan is stamped
where it lives). Read the stamp before dismissing a match as unrelated. An unset
`$QONTINUI_PLANS_DIR` skips this half and is **not** an error — (a) is the surface
of record.

If a close match exists, surface it to the user and ask whether to extend
the existing plan (open it and add a phase/section) instead of authoring a
duplicate — do NOT silently create a second plan for the same problem.

**Two clean misses are not the same as one.** Proceed only when (a) returned a
result you can trust — a populated corpus that did not contain it. If (a) was
UNKNOWN (an empty page with the body sync unconfirmed, no credential, or
qontinui-web unreachable) then a `$QONTINUI_PLANS_DIR` glob is the only check that
actually ran: **say so in your report, and say which layer you are relying on**,
rather than reporting "no existing plan" as though both agreed. On this fleet the
document layer has measured empty against hundreds of plans on disk, so treating
its silence as absence is how the same plan gets authored twice.

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

**The heading carries the TREE, not just the date** *(plan
`2026-09-05-a-verification-report-never-states-the-tree-it-read`, Phase 4)*.
`origin/main` moves under a plan continuously — that is this fleet's ordinary
condition, not an edge case — so every `file:line` and every "SHIPPED / still
open" judgement in this table is a statement about ONE commit. Two dates alone
cannot tell a reader whether the corpus moved between them; a sha can, in one
comparison, instead of a re-check of every row. This is measured on the plan
that added the rule: its own prior-art table was resolved at `bddef06`, and by
the time it was vetted the tree was `b579b45` with two of its PR citations gone
stale in that window. Read the sha with `git -C <repo> rev-parse --short
origin/main` after a `git fetch`, and name the repo — a bare sha names no
checkout.

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
> **Area:** `<area>` — work-unit `metadata.area`; kebab-case only,
> `[a-z0-9]+(-[a-z0-9]+)*`, in THIS blockquote; free text here is dropped with a
> warning. Omit the line when no area applies.

> **Repo(s):** <repo1>[, <repo2>...]

## Why
<the motivating problem, pulled from the prompt + your own research —
not a copy-paste of the prompt>

## Design decision(s) — <name the tradeoff, omit section if none>
<only for genuinely non-obvious choices; use a comparison table like
existing plans do (see `plans/2026-05-24-symbol-claim-tenant-scoping.md`
§"Design decision" for the shape) and end with **Resolved.** + the
deciding priority>

## Discovered prior art (verified <YYYY-MM-DD> against <repo> `origin/main` <short-sha>)
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

**A push is not the publication — land the plan with the helper.** A branch
pushed with no pull request, onto a repo that needs one, never reaches `main`:
the plan is published to `origin` and permanently invisible to every
`origin/main` reader, which is the population a peer vetter, `/preflight` and
the plan registry all read. Measured 2026-09-02, **9** plan
stems were pushed to `origin` and never proposed at all — no PR in any state, on
any branch carrying the stem — one of them a plan authored the day before by this
very step. So publish the new plan with:

```bash
bash <workspace-root>/qontinui-claude-config/scripts/land-plan-stamp.sh \
  "<plans-repo-root>" "<repo-relative plan path>" "<local plan file>" \
  "docs: add <plan-stem> (DRAFT)"
```

Whether the plans repo needs a PR is decided by the helper's ruleset probe of
its default branch, not asserted here. With no PR-requiring rule it lands the
blob directly and prints `LANDED <commit|unchanged> <blob>`; with one, or when
the probe cannot tell, it cuts a fresh branch, opens a new PR with
**`gh pr create`** — the only opener it runs — with a line-anchored
`Plan: <stem>` marker in the body, and prints `PROPOSED <pr-url|branch> <branch>`.
To open it with `coord_create_pr` instead, set `LAND_PLAN_STAMP_NO_PR=1`: the
helper pushes and reads back the branch, prints it, and opens nothing; then open
the PR with `coord_create_pr`, falling back to `gh pr create`.
It never pushes to an existing branch. A non-zero exit means the plan is NOT
published; report it. On `PROPOSED`, read the PR back with
`gh pr list --repo <owner/repo> --head <branch> --state all --json number,state,headRefOid,url`
— `--state all`, not `--state open`: an empty open-only answer cannot tell
"never proposed" from "closed under you". **Never `gh pr merge`, never
`--admin`** — coord is the sole merge authority. Runbook:
`knowledge-base/qontinui-specific/bodyless-work-units-and-stranded-plans.md`.

Note what publishing the plan does **not** by itself buy you: it does not make the
coord work unit's `vetted` status reachable.

**Do not restate the attestation rule in the plan you write.** Read it live and
link to it: `/policy get policy plan-discipline` (the Attested bullet and
`vetting-independence-is-context-independence`) and
`/policy get policy verification-and-evidence`
(`independence-is-context-not-credential`). A plan header that hardcodes the
mechanism goes stale the moment the operator moves it, and a stale rule read at
session boot for free beats a live one that costs a tool call — so the copy wins
and the plan misleads every later reader
[policy: never-pin-a-mutable-policy-value].

That has already happened. This command previously described a six-tier
`non_author_allows_identities` ladder over `{device, agent, session}` for the
work-unit check. **That was wrong about the code**: re-verified on qontinui-coord
`origin/main` 2026-09-23 at `037fc1a8f`,
`work_unit_registry::authorize_target_transition` takes the two actor keys
**plus an optional `independence` declaration** (`{verified, against, context}`),
and does the flat `owner == attester` compare **only when no declaration is
sent** — a well-formed one authorizes an Attested transition without that
compare, while still refusing `attester_unresolved` when the caller's token
derives no actor key; `non_author_allows_identities` is called only from
`gates.rs`. Fourteen plan status blocks now carry that invented ladder as fact.

⚠️ **Whether the coord instance serving YOU advertises that declaration is a
READ, never an assumption — and NOT the one your own tool list answers.**
Measured 2026-09-23: the authoring session's advertised
`coord_work_unit_transition` schema carried four properties while the door
carried six, so a session trusting its own tool list would have recorded "not
served" when the door said otherwise. Read the door — `coord-revive.sh tools`,
then look for `independence` in that tool's `inputSchema` — and take the worked
declaration from `/vet-plan` → `self_attestation_forbidden`; if your own tool
list is the stale one, send it with
`coord-revive.sh call coord_work_unit_transition '<json>'` rather than the tool
your session advertises, and verify by read — a zero exit is not evidence the
write landed. The declaration is STORED on the unit, so one you did not earn is
a false witness statement with your actor key beside it.

⚠️ **Never re-allocate to get past `self_attestation_forbidden`** — a fresh
allocate issues a NEW agent id the legacy compare would admit, which that
refusal itself names *"a known defect being tracked, not a sanctioned route"*.
That prohibition is that refusal's alone: `attester_unresolved` wants a device-
or agent-identified caller, which is a credential remedy rather than a route
around a control.

What you may safely rely on, because it is mechanism rather than policy: the
plan FILE's own status stamp is always yours to write, and the coord transition
is best-effort — if it refuses, record that the status write was skipped and
why, and never report the underlying work as blocked because a status string
could not be written [policy: bookkeeping-writes-waivable-at-the-floor].
Publish for reviewability, `conflict_check` and the durable record.

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
