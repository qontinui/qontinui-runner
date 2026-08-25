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

- **`$QONTINUI_PLANS_DIR`** — the directory plans live in. **If it is unset, ask the
  user once where plans live, or fall back to `<workspace-root>/plans`** (a `plans/`
  directory beside the repos this session is working in). Never assume an absolute
  path from another machine. It holds shipped and unshipped plans alike — status comes
  from the stamp, never from the directory.
- **`$QONTINUI_PLANS_ARCHIVE_DIR`** — optional, normally unset. When set and different
  from `$QONTINUI_PLANS_DIR`, it holds already-archived plans; search it when a stem
  does not resolve in the active directory. Archiving is a file location, not a
  lifecycle state — an archived plan still carries its own status stamp.
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
3. If `$QONTINUI_PLANS_ARCHIVE_DIR` is set and differs from `$QONTINUI_PLANS_DIR`,
   also try `$QONTINUI_PLANS_ARCHIVE_DIR/<stem>.md`; if still unresolved, report
   `MISSING`.

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
   all count.
2. Use `Edit` to **delete every existing status-adjacent blockquote** —
   even if a different skill wrote it (`/vet-plan` writes `VETTED`;
   `/implement-plan` writes `IN PROGRESS` / `SHIPPED`). Yours replaces
   all of them.
3. Then `Edit` again to insert your single new `> **Status:` block.
4. If folding in history is useful (e.g., the plan was previously
   stamped `VETTED` and your verify pass found it still
   `NOT STARTED`), include that in **one trailing line inside your
   new block**, prefixed `History:` or `Previously:`. Never as a
   sibling blockquote.

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
discovers a plan whose existing `VETTED` or `IN PROGRESS` stamp
disagrees with reality, write the more accurate state (`NOT STARTED`,
`PARTIAL`, `SHIPPED`) and capture the prior state in the History line.

#### Default behavior on already-stamped plans

By default, skip plans with an existing `> **Status:` block — they
have been triaged. List them in your final report. ONLY overwrite
when (a) the user explicitly asked for a re-verify, or (b) the
existing stamp is provably wrong (a referenced commit doesn't exist,
or a SHIPPED claim's files are missing). When you do overwrite, follow
the single-stamp invariant above.

### 6. Leave every plan where it is — the stamp is the archive

**This skill never moves a plan file.** A plan stamped SHIPPED / SUPERSEDED /
OBSOLETE stays in the exact directory it already occupies, exactly like PARTIAL and
NOT STARTED ones. The status block, not the location, records the outcome. Moving a
plan into `$QONTINUI_PLANS_ARCHIVE_DIR` — when a user has configured one — belongs to
`/implement-plan` Step 6, at the moment work actually ships; a verification pass is
read-only about location.

- In `$QONTINUI_PLANS_DIR` → leave it there and commit the status edit.
- In a `$QONTINUI_PLANS_DIR/../<plan-dir>/` suite → leave it there, commit the
  status edit, and flip its `00-index.md` row **if that directory has one**.
- In `$QONTINUI_PLANS_ARCHIVE_DIR` → leave it there too; an archived plan is stamped
  in place like any other.

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
If the sweep also stamped plans under `$QONTINUI_PLANS_ARCHIVE_DIR`, run the same
conditional block against that directory — it may be a different repo, or no repo.

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
```

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
