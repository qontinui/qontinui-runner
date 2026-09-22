# Verify Plan Status

Audit one or more plans against the source code to determine whether the plan
has been fully implemented, partially implemented, or not started — then stamp
a status block on the plan. Plans are stamped IN PLACE — never relocated.

## Arguments

- `$ARGUMENTS` — one of:
  - A plan file path (e.g. `plans/restate-port-part-c.md`) — verify just this one.
  - A directory path — verify every `.md` plan in that directory whose status
    is unknown (no `> **Status:` block at the top).
  - Empty — verify every plan in `$QONTINUI_PLANS_DIR` (see below) whose status is
    unknown. That can be the full corpus (hundreds of plans), so bound it explicitly
    if you only mean to spot-check. State the scope in the final report (e.g.
    "Verified N plans in `<dir>`") so a partial sweep never reads as a complete one.

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

- **`$QONTINUI_PLANS_DIR`** — the directory plans live in. **If it is unset, ask the
  user once where plans live, or DISCOVER one: from the workspace root,
  `ls -d plans */plans 2>/dev/null` and use the directory that actually exists** — say
  which, and ask when it finds none or more than one. Never fall back to a directory
  you have not confirmed is there; a named fallback fails silently on every machine
  that does not have it. Never assume an absolute
  path from another machine. It holds shipped and unshipped plans alike — status comes
  from the stamp, never from the directory.
- **Suite directories** — a multi-plan suite lives in its own directory *beside*
  `$QONTINUI_PLANS_DIR` (`$QONTINUI_PLANS_DIR/../<plan-dir>/`), with an optional
  `00-index.md`.

Neither directory has to be inside a git repo. Where this skill commits status edits
(§6) it first checks `git -C "<dir>" rev-parse --is-inside-work-tree`; when that fails,
the stamped files on disk are the whole ritual.

## Status block convention

A plan declares its state with a status block at the top:

```markdown
> **Status: SHIPPED <YYYY-MM-DD>.** <summary + commit SHAs>.
```

or `Status: PARTIAL` / `Status: SUPERSEDED by <other-plan>` / `Status: OBSOLETE`.

## Instructions

### 1. Inventory the targets

Resolve `$ARGUMENTS` to a list of plan files. For each file, read its first
~80 lines to extract:

- **Title** (H1).
- **Existing status block** if any (skip already-stamped plans).
- **`Depends-On:` field** (optional) — see [Depends-On lookup](#depends-on-lookup) below.
- **Acceptance criteria** — the plan's own "Definition of done", phase
  acceptance gates, "Files to add/modify" lists, "Migrations" sections.
  These are the verifiable claims you'll check against the code.

Skip plans that already have a `> **Status:` block — those have been triaged.
List them in your final report so the user can spot any whose status drifted.

#### Depends-On lookup

A plan MAY declare upstream dependencies inline in its status blockquote
using a `Depends-On:` suffix:

```markdown
> **Status: VETTED 2026-05-21.** <summary>. Depends-On: 2026-05-20-default-tenant-propagation, 2026-05-19-some-other-plan.
```

Parser rule:

1. Look at the status blockquote (the first `> **Status:` block under the
   H1) and find EVERY case-sensitive `Depends-On:` occurrence — a block
   often carries one in the headline sentence and another in a trailing
   `History:` / re-vet line.
2. For each occurrence, consider only the remainder of that PHYSICAL line
   — never the following blockquote lines or paragraphs, which may name
   unrelated plans in prose.
3. Within that line, keep only date-prefixed plan-stem-shaped tokens
   (`YYYY-MM-DD-<kebab-slug>`, e.g. `2026-06-02-some-plan`). Prose, bare
   dates (`2026-05-21.`), and trailing punctuation never produce tokens
   — a stem requires at least one `-word` segment after the date. Each
   token is a bare plan **stem** — no `.md` extension, no path.
4. Union the stems across all occurrences, deduped, order-preserving.

   (A naive first-occurrence + split-on-commas parse mis-handled real
   status blocks whose prose contained a second `Depends-On:` or commas —
   it produced phantom missing-dep aborts. Fixed in the canonical resolver
   2026-06-04; this inline fallback mirrors it.)

For each dep stem, resolve to a plan file using the [Plan stem resolution
chain](#plan-stem-resolution-chain) below. Capture each dep's status (read
the dep file's status blockquote and parse the lifecycle word — one of
`DRAFT`, `VETTED`, `IN PROGRESS`, `SHIPPED`, `PARTIAL`, `NOT STARTED`,
`SUPERSEDED`, `OBSOLETE`). If the dep file can't be found in either
directory, record the dep as `MISSING (no plan file)` and surface it as a
drifted-status item in the final report.

A plan without a `Depends-On:` field is the common case — proceed with no
dep checks.

#### Plan stem resolution chain

To turn a bare stem (e.g. `2026-05-20-default-tenant-propagation`) into a
plan file path:

1. Try `$QONTINUI_PLANS_DIR/<stem>.md`.
2. If that doesn't exist, check the suite dirs beside it
   (`$QONTINUI_PLANS_DIR/../<plan-dir>/`).
3. If still unresolved, report `MISSING`.

Use `Read` (with the explicit absolute path; a `Read` failure is the
not-found signal) or `Glob` (`$QONTINUI_PLANS_DIR/<stem>.md`) to check.

### 2. Build a verification checklist per plan

For each plan, extract a small set (5–15) of verifiable claims. Examples by claim type:

- **"Files to add" or "New module"** → use Glob/Read to confirm the file exists
  and has the structure described.
- **"Migration vN"** → grep `MIGRATIONS` in `database/pg/mod.rs` for the version
  number; check `schema.pg.sql` for the same DDL.
- **"New endpoint POST /foo/bar"** → grep `mcp` modules for the route
  registration; confirm a handler function exists.
- **"New slash command /foo"** → confirm `.claude/commands/foo.md` exists.
- **"New Tauri command foo"** → grep `generate_handler!` in `src-tauri/src/main.rs`
  for the symbol.
- **"New React component Foo.tsx"** → confirm the file exists and is mounted
  in a parent component.
- **"Phase N — Foundation: ..."** → check the phase's "Definition of done"
  bullet list, treating each bullet as a sub-claim.

Do NOT require behavioral runtime checks — that's `/manual-test`'s job. This
skill verifies code-level shipment, not runtime correctness.

### 3. Verify each claim

For each claim, run the cheapest possible check that confirms or denies it:

- Glob for files mentioned by path.
- Grep for symbol names, route strings, migration version numbers, table names.
- Read 10–30 lines around any hit to confirm the structure matches what the
  plan describes.
- Cross-reference with `git log --oneline -- <path>` if you need to identify
  the commit that landed the change.

If a plan references commits in its body (e.g. "shipped in 879c36bb4"),
confirm the SHA exists with `git rev-parse 879c36bb4 2>/dev/null` and is
in the current branch's history with `git merge-base --is-ancestor 879c36bb4 HEAD`.

> **Canonical "is it shipped / landed?" check — ask the twin, not a local tree.**
> Before deciding a plan or PR has (or hasn't) landed, call the
> **`coord_query_delivery`** MCP tool (HTTP: `GET
> /coord/twin/delivery/verdict?plan_slug=<stem>` on the SSO surface). It returns
> a `DriftVerdict` (`instance="delivery"`) that joins the plan's lifecycle
> status ⋈ its cited PRs' merged-state (observed against **origin**, not your
> checkout) ⋈ best-effort deploy state — **with `staleness_seconds`**, so a
> stale answer is visibly stale. Read `components.status`, `components.prs[]`
> (`{repo,pr,merged}`), `components.all_merged`, and `drift_class`
> (`delivery:shipped_but_unmerged` = stamped shipped but a cited PR is still
> open; `delivery:merged_but_unstamped` = PRs merged under a not-yet-shipped
> plan). NEVER judge landed-state from a local working tree — a local tree can
> be days stale (the 2026-06-15 stale-checkout incident this tool was built to
> prevent; pair with the `fetch-origin-before-judging-landed-state` lesson). The
> tool observes origin so you don't have to fetch-and-guess. The local `git`
> checks above remain valid for locating *which commit* implements a claim once
> the twin confirms it landed — they are not the authority on *whether* it
> landed.

For plans that span multiple repos (productivity-stack touches qontinui-runner,
qontinui-navigation, ui-bridge, .claude, qontinui-dev-notes), check each repo's
log independently.

### 4. Categorize the plan

Based on the verification, place each plan in one of:

- **SHIPPED** — every claim verified; no open phase, no missing file/symbol/endpoint.
- **PARTIAL** — some claims verified, some missing. Note which.
- **NOT STARTED** — zero claims verified. The plan is still aspirational.
- **SUPERSEDED** — the plan's body says it's superseded by another plan, OR
  the work is in main but not via the path the plan describes (a different
  approach landed). Cite the superseding artifact.
- **OBSOLETE** — the work is no longer applicable (technology removed,
  feature cancelled, etc.). The body usually says so explicitly.
- **IN FLIGHT — guard held** — the plan is stamped `IN PROGRESS` and §5's
  conditional guard refused the overwrite (case 3, a live peer; or the
  unidentified default). This is a **refusal, not a verdict**: you did not
  verify the plan this pass, and you must not report it as `NOT STARTED`
  merely because you found no evidence — that is the laundering the guard
  exists to stop.

### 5. Stamp a status block

For each plan, edit the .md to insert a status block immediately below
the H1:

```markdown
# <Plan Title>

> **Status: SHIPPED <YYYY-MM-DD>.** <1–3 line summary of what's live>.
> Commits: <repo>@<sha> (<short msg>); <repo>@<sha> (...).
> [Followup file: <relative path> if any.]

<rest of plan body unchanged>
```

For PARTIAL:
```markdown
> **Status: PARTIAL <YYYY-MM-DD>.** Phases <X>, <Y> shipped (commits <sha>,
> <sha>). Phase <Z> open: <one-sentence reason>. <Followup ptr if any.>
```

For NOT STARTED:
```markdown
> **Status: NOT STARTED (verified <YYYY-MM-DD>).** No source-code evidence
> of any acceptance criterion. <Why it might still be worth doing OR why it's
> stale — one sentence either way.>
```

For SUPERSEDED / OBSOLETE: cite the replacement or the reason.

#### Single-stamp invariant — read before stamping

A plan must have **exactly one** `> **Status:` blockquote between the H1
and the body. Before writing your stamp:

1. Read the top of the plan. Identify EVERY top-of-file blockquote that
   asserts a status, lifecycle state, or verification date — lines
   starting `> **Status:`, `> **Edit YYYY-MM-DD —`, or `> **Update:`
   all count. **Record verbatim any block reading `IN PROGRESS`** — its
   token, its date and its session marker — before you touch anything.
   This step is the last point at which that marker is guaranteed
   readable, and the guard below cannot discriminate anything without it.
2. Use `Edit` to **delete every existing status-adjacent blockquote** —
   even if a different skill wrote it (`/vet-plan` writes `VETTED`;
   `/implement-plan` writes `IN PROGRESS` / `SHIPPED`). Yours replaces
   all of them. **One exception: a block reading `IN PROGRESS` is not
   yours to delete until the conditional guard below has cleared it** —
   see "Overwriting `IN PROGRESS`".
3. Then `Edit` again to insert your single new `> **Status:` block.
4. If folding in history is useful (e.g., the plan was previously
   stamped `VETTED` and your verify pass found it still
   `NOT STARTED`), include that in **one trailing line inside your
   new block**, prefixed `History:` or `Previously:`. Never as a
   sibling blockquote.
5. **Carry the `**Area:**` and `Depends-On:` declarations.** If the block you
   are replacing carries a `` > **Area:** `<area>` `` line or a `Depends-On:`
   declaration, carry each verbatim into your new block — `Depends-On:` as the
   suffix of your new `> **Status:` line (or wherever in the block it stood),
   `**Area:**` as a `>` line (conventionally directly under the Status line).
   Either must stay inside that same blockquote: no blank line or non-`>` line
   between it and the `> **Status:` line, and never a sibling blockquote.
   The runner's plan scanner reads both only from this status
   blockquote, into `metadata.area` and `metadata.depends_on`, and coord
   replaces the pushed `metadata` on every upsert, so a stamp that drops
   either erases it from the work unit on the next scan.

When `/verify-plan-status` finds existing stamps that disagree with
what you'd verify (e.g. a plan stamped `VETTED` whose acceptance
criteria still aren't shipped), consolidate: keep the more useful
lifecycle indicator in the heading (`Status: VETTED — implementation
not started.` or `Status: NOT STARTED.`) and put the orthogonal
finding in the body. Don't leave both.

#### Lifecycle states this skill writes

| State | When |
|---|---|
| SHIPPED | every claim verified live in code |
| PARTIAL | some phases shipped, others open |
| NOT STARTED | zero source evidence of any claim |
| SUPERSEDED | a different approach landed; cite the replacement |
| OBSOLETE | the work no longer applies |

This skill does NOT write `DRAFT`, `VETTED`, or `IN PROGRESS` — those
are owned by `/vet-plan` and `/implement-plan`. If `/verify-plan-status`
discovers a plan whose existing `VETTED` stamp disagrees with reality,
write the more accurate state (`NOT STARTED`, `PARTIAL`, `SHIPPED`) and
capture the prior state in the History line. **`IN PROGRESS` is the one
exception: that stamp is CONDITIONALLY overwritable, never freely, and the
next subsection governs when.**

##### Overwriting `IN PROGRESS` — conditional, because this skill can LAUNDER it

This skill is the writer the laundering failure was first measured on (#485):
it downgraded a guarded `IN PROGRESS` to `NOT STARTED`, a token every other
writer declares freely overwritable. The shared guard block states the rule;
the subsection after it is this skill's own disposition.

<!-- status-guard:start -->
> **`IN PROGRESS` is a GUARDED STATE — it is never freely overwritable.**
> *(Roster and gate: `.claude/commands/_status-writers.md`, check #64. The full
> arm table and its evaluation order: `/vet-plan`, "`IN PROGRESS` is
> CONDITIONALLY overwritable".)* Before this command writes, replaces,
> downgrades or re-dates a plan's lifecycle stamp, read the stamp already there
> and apply these five rules.
>
> 1. **An `IN PROGRESS` stamp is a conditional STOP, not a value to replace.**
>    It protects a live peer's in-flight work.
> 2. **The discriminator is the SESSION MARKER, not the token.** A marker that
>    IS your own current session id is a resume: refresh the date and keep the
>    trail, never take over. A marker that is a different session id is a LIVE
>    PEER unless you can positively verify that session died with zero work
>    products — transcript tail shows death, its worktrees clean and 0 ahead of
>    `origin/main`, and no PRs and no branches for the plan. Verified dead, and
>    only then, adopt it and append your own marker.
> 3. **No marker, or one you cannot positively attribute, is the UNIDENTIFIED
>    DEFAULT: STOP.** Not an overwrite, and not an adoption. Adoption is the
>    earned branch; stopping is the fallback.
> 4. **Do not LAUNDER it.** Rewriting the token into `NOT STARTED`, `PARTIAL`,
>    `DRAFT`, a terminal state or a bare re-date converts a hard STOP into a
>    state the other writers declare freely overwritable, and every downstream
>    reader then sees a well-formed stamp written by a trusted skill. That is
>    this guard's failure mode: laundering, not bypass (#485).
> 5. **An UNKNOWN is not permission.** A delivery read that is degraded, masked,
>    non-2xx, unparseable or carrying `merged_degraded_reason` leaves the
>    stamp's meaning unestablished. Fall through to STOP, never to overwrite,
>    and say the read was inconclusive.
<!-- status-guard:end -->

`IN PROGRESS` is the only lifecycle token whose overwrite is **load-bearing in
another command**. `/vet-plan` §5 ("`IN PROGRESS` is CONDITIONALLY
overwritable") and `/implement-plan` Step 0.5 both treat it as a conditional
**STOP**: a stamp whose session marker is not positively attributable to the
reading session is a live peer, or an unattributable stamp, and both stop the
run. Those same two commands declare `PARTIAL` and `NOT STARTED` **freely**
overwritable — `/vet-plan` §5: "`PARTIAL` and `NOT STARTED` are fine to
overwrite"; `/implement-plan` Step 0.5: "replace it with the IN PROGRESS
block".

So a verify pass that rewrites a peer's `IN PROGRESS` to `NOT STARTED` does not
merely record a status. It **converts a hard STOP into a permissive arm** and
deletes the session marker that was the only discriminator — the same evidence
destruction `/vet-plan` Step 0.25 was moved ahead of §4 to prevent, reached
through a different door. The downstream commands cannot detect it: what they read
afterwards is a well-formed `NOT STARTED` stamp written by a trusted skill.

**`NOT STARTED` is also the wrong reading for a live peer, by construction.**
This skill defines `NOT STARTED` as *"zero source evidence of any claim"* —
which is exactly what a peer's un-pushed, uncommitted work looks like from
outside its worktree. Absence of evidence is UNKNOWN here, not a verdict
(`verification-and-evidence` `silent-empty-is-unknown`).

**So: before overwriting a status block that reads `IN PROGRESS`**, use the
block you recorded at the single-stamp invariant's sub-step 1 and apply
`/vet-plan` §5's disposition table.

`/vet-plan` §5 opens with a per-command table answering "is the stamp still
readable when you get here?", and it has no row for this command. **The answer
for `/verify-plan-status` is YES** — §5 is this skill's first write, nothing
before it edits the plan, so unlike `/vet-plan` (whose §4 has already rewritten
the file) the stamp is intact and you run the arms inline, as
`/implement-plan` does at its Step 0.45 check 1. Do not go looking for a
capture step this command does not have.

The cases map onto this skill's verbs:

| `/vet-plan` case | Disposition here |
|---|---|
| 0 — the marker **is your own** current session id | **Reachable, and not rare** — the marker names a *session*, not a command, and one session routinely runs `/implement-plan` (which stamps it) and later `/verify-plan-status`, directly or through `/pvi` or `/vet-imp`. There is no peer to protect here: **write the verified state freely**, naming the prior stamp as your own in the `History:` line. Evaluate this row FIRST — it is a positively attributed marker, so it must never fall to the unidentified default, which is defined by the absence of exactly that attribution. |
| 1 — the work has **landed** (delivery arm 1) | The plan is closed in substance. Write **`SHIPPED`** — a state this skill owns — when your own evidence scan agrees. If it does **not** agree, that disagreement IS the finding (a `delivery:` drift class): report it and leave the block alone. Never resolve it by writing `NOT STARTED` or `PARTIAL` over the marker. **Deliberate divergence, do not "restore consistency":** `/vet-plan` case 1 says *refuse and route to closeout* because `/vet-plan` writes only `VETTED`. Writing `SHIPPED` here IS that closeout — §5's own closeout paragraph assigns the plan-file terminal status to whoever can write it ("always yours to write"), and `SHIPPED` is this skill's state. Flipping this row to "refuse" would delete the only path by which a landed plan ever gets closed. |
| 2 — the stamping session is **dead with zero work products** (its transcript tail shows death, **and** its worktrees are clean and 0 ahead of `origin/main`, **and** no PRs and no branches exist for the plan) | Free to write the verified state. Name both the prior marker and the death evidence in the `History:` line. |
| 3 — a **live peer** (marker ≠ yours and case 2's checks do not all hold) | **Do not overwrite.** Leave `IN PROGRESS` standing and report the plan as *in flight — not verifiable this pass*. |
| — **unidentified default**: no marker at all, or one you cannot positively attribute | **Do not overwrite**, exactly as case 3. An absent marker cannot reach case 2, because case 2's probes are keyed on a session id and there is none to probe. |

**Settling case 1 — and this command has no mandatory capture step, so there
are FOUR branches, not three.** The `coord_query_delivery` twin documented
above settles case 1 on its own: a `delivery:merged_but_unstamped` verdict on a
plan stamped `IN PROGRESS` **is** case 1.

- **You already issued the read** (§3 sent you there) — consume it. Do not
  issue a second one.
- **You have NOT issued one.** Issue it now. This is the ordinary path: §3
  scopes its read to "before deciding a plan or PR has (or hasn't) landed",
  which a sweep may never have reached for this plan.
- **The tool is not visible** — masked, absent, or on a dead transport
  (`"Command failed with no output"`) — or the read is unreachable,
  unparseable, or non-2xx. **UNKNOWN.**
- **Anything else.** UNKNOWN.

**UNKNOWN lands on the unidentified default (do not overwrite), never on "not
started"** — `verification-and-evidence` `unknown-must-not-render-as-a-default`.
Case 1 is the only arm that lets this skill write over an `IN PROGRESS` plan at
all, so silently skipping the read does not fail safe *into permission*; it
fails closed, leaving the stamp alone.

**The exit, so this STOP is a guard and not a trap.** After this rule an
unmarked `IN PROGRESS` stamp — hand-written, operator-written, or predating the
marker convention — is refused by `/vet-plan` Step 0.25, `/implement-plan`
Step 0.5, `/vet-imp`, and now this skill. That is deliberate, and it is **not** a
dead end: it is corrected by the operator editing the stamp by hand, or by the
owning session resuming through `/implement-plan`, whose case 0 lets a session
refresh its **own** marker. Report the plan and name that exit. Do not
rationalize your way past the STOP because no automated corrector remains — an
unexitable-looking guard is exactly the condition under which
[[feedback_plan_already_stamped_by_other_session_is_live_peer]] was talked
past, at the cost of a full wasted implementation (PR #479 against PR #468).

`DRAFT`, `VETTED`, `PARTIAL` and `NOT STARTED` get none of this treatment —
nothing downstream reads them as a peer-liveness signal. This subsection is
scoped to `IN PROGRESS` alone.

#### Default behavior on already-stamped plans

By default, skip plans with an existing `> **Status:` block — they
have been triaged. List them in your final report. ONLY overwrite
when (a) the user explicitly asked for a re-verify, or (b) the
existing stamp is provably wrong (a referenced commit doesn't exist,
or a SHIPPED claim's files are missing). When you do overwrite, follow
the single-stamp invariant above.

**Neither (a) nor (b) lifts the `IN PROGRESS` guard.** A user asking for a
re-verify sweep is not attribution evidence about the stamping session, and
"provably wrong" is not establishable for a peer whose work is still unpushed —
that is the case the guard exists for. An `IN PROGRESS` block still needs its
disposition before it can be replaced.

### 6. Leave every plan where it is — the stamp is the archive

**This skill never moves a plan file.** A plan stamped SHIPPED / SUPERSEDED /
OBSOLETE stays in the exact directory it already occupies, exactly like PARTIAL and
NOT STARTED ones. The status block, not the location, records the outcome — and
there is no archive directory to move anything into.

- In `$QONTINUI_PLANS_DIR` → leave it there and commit the status edit.
- In a `$QONTINUI_PLANS_DIR/../<plan-dir>/` suite → leave it there, commit the
  status edit, and flip its `00-index.md` row **if that directory has one**.

> **Why:** this step used to `mv` shipped plans out of a separate untracked working
> directory into a git-tracked one. A later cleanup commit deleted five plans — three
> moved by that rule, plus two unrelated DRAFTs — and because the `mv` had already
> removed the three from the untracked source, those records existed nowhere on disk
> until they were recovered by hand (operator incident, 2026-07-21). The general rule:
> **a verification pass never relocates a plan**, and a plan only ever moves into a
> location at least as durable as the one it left. When the plan directory is a git
> repo, recovery is always possible:
> ```bash
> cd "$QONTINUI_PLANS_DIR"
> # newest deletion first (a plan may have been deleted and re-added):
> git log --diff-filter=D --oneline -1 -- <name>.md   # -> <del-commit>
> git checkout <del-commit>^ -- <name>.md             # atomic restore
> ```
> For a suite-dir plan, swap the path for `../<plan-dir>/NN-<name>.md`.

Commit the status edits — one batch commit, **only if the plan directory is inside a
git repo**:

```bash
if git -C "$QONTINUI_PLANS_DIR" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  # Name the stamped paths explicitly — a shared checkout's index may hold a peer's
  # staged files, and a bare `git add -A` would publish them.
  git -C "$QONTINUI_PLANS_DIR" commit \
    -m "docs(plans): status-stamp <N> verified plans" \
    -m "Verified against source <date>. SHIPPED: <list of plan names>. PARTIAL: <list>. NOT STARTED: <list>." \
    -- <stamped paths>
  git -C "$QONTINUI_PLANS_DIR" push
fi
```

If the check fails, the plan directory is a plain folder: the stamped files on disk
are the record, there is nothing to commit or push, and you must not create a repo to
hold them. (Closeout push authority covers docs/plans diffs wherever a repo exists.)

### 7. Final report

Single message back to the user:

```
Verified <N> plans in <scanned dir(s)>.

SHIPPED:
  - <plan>.md — <one-line shipment summary>
  ...

PARTIAL:
  - <plan>.md — Phase X done, Phase Y open
  ...

NOT STARTED:
  - <plan>.md
  ...

SUPERSEDED:
  - <plan>.md — replaced by <other>.md
  ...

ALREADY STAMPED (skipped):
  - <plan>.md — declared <status>
  ...

IN FLIGHT — guard held (not verifiable this pass):
  - <plan>.md — IN PROGRESS <date>, marker <marker, or "none">; case 3 / unidentified
  ...
```

**`IN FLIGHT — guard held` is a separate bucket on purpose; folding it into
`ALREADY STAMPED (skipped)` defeats the guard.** A refusal that reports
identically to routine triage is indistinguishable from never having run the
check — which makes the guard vacuously satisfiable and hides a live peer from
the one reader who could act on it. Carry the marker (or `none`) and the case:
those are the discriminator, and without them the next run re-derives from
scratch what this one already established.

For every plan that declared `Depends-On:` (whether you stamped it this
pass or skipped because it was already stamped), append a `Dependencies:`
sub-block under that plan's report line, listing each dep's stem, current
lifecycle status, and resolved location:

```
  Dependencies:
    - 2026-05-20-default-tenant-propagation — SHIPPED (plans dir)
    - 2026-05-19-some-other-plan — IN PROGRESS (plans dir)
    - 2026-05-18-removed-plan — MISSING (no plan file)
```

This makes upstream/downstream graph state legible from a single report
line without forcing the operator to crawl the dep tree by hand.

If any plan's stamped status disagrees with what you'd verify now (e.g. the
file says SHIPPED but a referenced commit doesn't exist on main, or a
`Depends-On:` token has no matching plan file), flag it as a "drifted
status" item in the report so the user can correct it.

## Rules

- **Read-only on plan content; only edit the status block.** Don't rewrite or
  reorganize the plan body. Don't fix typos. Just add/update the status block.
- **Don't run runtime checks.** `cargo run`, `npm run dev`, manual-test, etc.
  are out of scope. This is static verification of source-code presence only.
- **Don't fix incomplete plans.** If you find PARTIAL or NOT STARTED plans,
  do NOT start implementing them. The output is a status report, not a fix.
- **Cross-repo plans require cross-repo grep.** If a plan touches qontinui-web
  or ui-bridge or .claude commands, scan those repos too — don't just check
  qontinui-runner.
- **Per `feedback_no_destructive_git.md`:** no repo-wide stash/checkout/reset.
  **And never `mv`/`git mv` a plan file at all** — plans are stamped in place
  (§6); there is no in-progress → completed migration.
- **Don't touch in-progress plans you can't verify.** If a plan's claims are
  too vague to check (e.g. a brainstorm doc), stamp it `Status: BRAINSTORM
  (verified <date>) — no concrete acceptance criteria; not actionable as-is`
  and leave it where it is.

## Implementation Notes

$ARGUMENTS
