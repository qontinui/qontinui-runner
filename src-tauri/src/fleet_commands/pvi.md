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

- **A dossier reference** — `dossier:<slug>`, or the literal head key
  `DOSSIER <slug>` — meaning *"author the remediation plan for this
  recurring-defect dossier"*. A dossier is the durable issue file
  `/unattended` 2b-bis maintains for a defect that keeps recurring despite
  shipped fixes (plan
  `2026-08-25-dossiers-durable-issue-files-for-recurring-defects`); Step 0
  resolves its live head and Step 1 hands the head — not the operator's
  recollection of it — to `/create-plan`. This shape is not forwarded
  verbatim: the prompt Step 1 forwards is built from the head.

## Plan directories

This command does not resolve plan directories itself — `/create-plan` owns that,
and Step 3 hands `/vet-imp` an already-resolved **absolute** path. The one place
this command touches a directory is Step 2's fallback `Glob`, which uses the same
variable `/create-plan` writes into:

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

- **`$QONTINUI_PLANS_DIR`** — the directory plans live in. The qontinui runner
  injects it into agent sessions from its `paths.plans_dir` setting; a session
  launched outside the runner will not have it. **If it is unset, ask the user once
  where plans live, or DISCOVER one: from the workspace root,
  `ls -d plans */plans 2>/dev/null` and use the directory that actually exists** —
  never fall back to a directory you have not confirmed is there, because a named
  fallback fails silently on every machine that does not have it — and use the *same*
  answer the Step 1 subagent used, never a different guess.

Step 2's fallback is a recovery path, not the normal one: prefer the absolute path
the subagent reports.

## Instructions

### Step 0 — Preflight: is this prompt already resolved?

**Run this before spawning anything.** It is two file reads (two store probes,
for a dossier) and it prevents the
most expensive failure this command has: writing a fresh plan for work that
already shipped.

A `prompts/` file is an **inbox item with no lifecycle of its own — nothing
expires it**. Once its work ships it keeps reading like live work forever.
Measured cost when skipped: on 2026-08-19 a `/pvi` run on
`2026-08-04-sccache-daemon-wedge-and-s3-regression.md` spent a full cycle
re-deriving a plan root-caused 13 days earlier whose fix was already installed.

**A dossier argument.** Evaluate this arm first, before the prompt-file
checks — a dossier is neither a file nor free text, and it carries its own
record of what has already been tried. Resolve the **live head**, in this
order, and stop at the first that answers:

1. `coord_recent_findings(topic="dossier:<slug>", kind="dossier")` — HTTP twin
   `GET /coord/agent-findings?topic=dossier:<slug>&kind=dossier`. Read
   `available` before `count`. A head here is authoritative.
2. On the **kind rejection** (`kind` refused as an unknown argument, or a
   `kind="dossier"` write answering `invalid kind` — the serving coord predates plan
   `2026-08-25-dossiers-durable-issue-files-for-recurring-defects` Phase 2;
   say so) **or an empty answer**, the memory key:
   `coord_memory_search(query_text="DOSSIER <slug>", kinds=["mental_model"])`,
   taking the newest `created_at` among live rows whose title begins exactly
   `DOSSIER <slug> —` — a `DOSSIER-CONTRIB` / `DOSSIER-DELTA` row is never the
   head. `coord_memory_search` is full-text only (read `vector_arm`), so add at
   least two probes in the issue's own words, as `/unattended` 2b-bis requires.
3. **An empty answer from every probe is UNKNOWN, not "no such dossier".**
   Pair it with `coord_memory_overview` and read `live_row_count` before
   concluding — a zero against a populated store is a miss, a zero against an
   empty or unreadable store is UNKNOWN — exactly as `/unattended` 2b-bis does.
   A miss means the slug is misspelt or the dossier was never opened: name
   the probes you ran and their answers, and do not author a plan against a
   dossier you could not read.

Then read the head's `artifact_refs.remediations` (a finding head) or its
ledger prose (a memory head). **If it already names a plan for this slug whose
status is not SHIPPED / SUPERSEDED / OBSOLETE, that is the same-stem hit this
preflight exists to catch**: route to that plan via the disposition below and
do not author a second. Verify that plan's status against `origin/main` — the
dossier's copy of it can lag — and treat a `held: true` remediation as still
open. A SHIPPED remediation is *not* a hit: it is prior art the plan must
account for (Step 1). An `IN PROGRESS` remediation IS a hit, and the one that
needs care: a live peer may hold it. That stamp is CONDITIONALLY overwritable
only under `/vet-plan`'s own rule for an `IN PROGRESS` stamp, which this skill
neither restates nor bypasses — route to that plan, but leave its stamp and its
open residuals to the session holding it unless that rule makes the stamp yours.

Carry the head's id (`finding_id`, or the memory id for a fallback-shape head),
title, body and ledger into Step 1.

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
   the normal gate (commit → PR → CI → coord merge train) — unless the plan is
   `IN PROGRESS` and not yours per the preflight note above, in which case
   report it and stop; its stamp and residuals belong to the session holding it.
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

Let it do its own research and write the plan file.

COMMIT THE PLAN YOURSELF, BEFORE YOU REPLY — and commit it in a worktree
you allocate, never in the checkout you started in. You do not have a
worktree; get one first:

    bash <workspace-root>/qontinui-claude-config/scripts/allocate-worktree.sh \
      --repo <the plans repo> --intent "<plan stem>"

Handle an `isolation.mode` of `wait` / `shared_branch` as that script
documents, and only on HTTP 409 `repo_not_registered` or an unreachable
coord fall back to `git -C <repo> worktree add -b <branch>
<workspace-root>/<repo>-wt-<slug> origin/main` — saying in your reply that
the worktree is undeclared.

Do not return an uncommitted plan and leave the commit to me.
`/create-plan` §5 already requires write -> commit -> push at creation,
stamped `DRAFT`; you are the session that must actually perform it. A plan
that exists only as untracked bytes is destroyed the moment that worktree is
removed (`git worktree remove --force`, or a fleet reaper deleting the
directory — after which `git worktree prune` tidies away the last record
that it existed), and it is invisible to `conflict_check`, to the plan
registry and to every peer until it is committed AND pushed.

When it's done, reply with ONLY: the plan's absolute file path, its title,
phase count, the repo(s) it touches, and the commit sha + branch you
committed it on. Do not include the discovered-prior-art table, the phase
details, or any other plan content in your reply — the caller only needs
the path. Begin that reply with the line
`FINAL-REPORT label=create-plan status=WRITTEN commit=<sha> branch=<branch>`
— first line, plain text; a reply that does not begin with it is read as a
progress note and you are resumed. Keep that line to bare `key=value`
tokens with no spaces in any value: the fleet sentinel parser
(`scripts/agent-report-verdict.sh`) rejects the whole line otherwise and
your report degrades to a progress note.

Two other terminal values, each with its prose on the NEXT line, never on
the sentinel: `status=FAILED` (nothing usable was produced) and
`status=UNCOMMITTED` (you wrote the file but could not commit it — give the
absolute path and the reason on the following line, so I can rescue the
bytes before your worktree goes away). Never report `WRITTEN` for a plan
you did not commit.
```

**When the argument was a dossier**, the prompt carries the head instead of
`$ARGUMENTS` — the head's **title, body and ledger verbatim** (the
`artifact_refs` JSON for a finding head; the ledger section for a memory head)
plus these instructions to `/create-plan`:

```
This prompt is the live head of dossier `<slug>` (id <finding_id or memory
id>), pasted verbatim below. Author the remediation plan for it.

- Treat EVERY entry in its `remediations` as prior art and read each one. A
  SHIPPED remediation that did not stop recurrence is the MOST VALUABLE input
  to this plan, not a reason to skip it: the plan must say why that fix did
  not close the issue and what this one does differently.
- Author against the head's analysis and candidate-directions sections — the
  synthesis is the problem statement; do not re-derive it from the evidence.
- Put a `Dossier: <finding_id or memory id>` pointer line in the plan's
  status block, so a session arriving via the plan finds the dossier.
```

The same reply contract applies — path, title, phase count, repos, and the
commit sha + branch, and nothing else. The commit is required on this arm
too: occurrences 7-9 of `dossier:stranded-plan-pr` all came through the
dossier arm.

### Step 2 — Extract the plan path

Parse the subagent's reply for the plan's absolute path. If it's missing or
ambiguous, `Glob` `$QONTINUI_PLANS_DIR/*.md` (see [Plan directories](#plan-directories))
sorted by mtime and take the most recently modified file (it should be the one Step 1
just created — confirm its title matches what the subagent reported before trusting
it). Resolve that glob to a concrete absolute path before Step 3 — `/vet-imp` and the
skills below it must receive a real path, never an unexpanded variable.

Then confirm the file is committed and pushed. `/create-plan` authors the plan in
a worktree (never the primary/shared checkout) and commits it at creation stamped
`DRAFT`, because `/vet-plan` cannot attest a plan no peer session can read.

**Verify that commit; do not take `status=WRITTEN` for it.** Step 1's reply
contract carries `commit=<sha> branch=<branch>` precisely so this step has
something to check rather than something to assume. Nothing mechanical enforces
it — `scripts/agent-report-verdict.sh` ignores unknown `key=value` tokens by
design, so a `status=WRITTEN` with no `commit=` still classifies as a clean
`FINAL_REPORT`. This hand-check is the only enforcement there is.

Derive the repo root and the repo-relative path from the absolute path the
subagent reported — never assume a `plans/` prefix, since this command does not
resolve plan directories (see [Plan directories](#plan-directories)) and the
`rev:path` form needs a **repo-root-relative** path:

```bash
repo=$(git -C "$(dirname "$plan_path")" rev-parse --show-toplevel)
rel=$(realpath --relative-to="$repo" "$plan_path")
git -C "$repo" fetch --quiet origin || true     # a sha you have not fetched is not a missing sha
git -C "$repo" cat-file -e "$sha:$rel"          # commit exists AND contains the plan
git -C "$repo" branch -r --contains "$sha"      # non-empty => it was actually PUSHED
```

Read the three results separately, because they fail for different reasons and
only one of them means "rescue":

- **No `commit=` in the reply, or `status=UNCOMMITTED`** → the plan is untracked
  bytes. **Rescue immediately**, before anything else: copy the file out of the
  subagent's worktree, then allocate your own worktree
  (`scripts/allocate-worktree.sh`) and commit and push it there — never in the
  checkout you are running in. The subagent's worktree can be removed out from
  under you and the bytes go with it.
- **`cat-file` fails after a successful fetch** → treat as UNCOMMITTED and
  rescue. Before the fetch it is UNKNOWN, not missing: a linked worktree shares
  the object store but a separate clone does not, and declaring UNCOMMITTED for
  a plan that is merely unfetched lands a SECOND copy on a second branch — the
  duplicate-plan class `/preflight` exists to prevent.
- **`cat-file` succeeds but `branch -r --contains` is empty** → committed but
  **not pushed**. Do not re-commit; just push that branch, then carry on to the
  PR check below.

> **Why this is a checked fact and not advice.** Measured 2026-09-13 on
> merytshost: three plan documents authored 2026-09-03 — 32 KB, 47 KB and 32 KB,
> fully phased — existed on **no git ref at all**. In each case the branch AND
> the worktree had been created *for that plan*, so the authoring session got as
> far as naming its work and then never ran the commit; no PR was ever opened.
> They survived only because a dossier sweep walked worktree state. Each was the
> `/pvi` output of a *different* dossier, so one missed commit was silently
> holding up three unrelated remediations. `dossier:stranded-plan-pr`
> occurrences 7-9; recovered as `qontinui-dev-notes#1127`. The doctrine was
> already written in `/create-plan` §5 and in this step — what was missing was
> anything that **verified** it, which is why the instruction moved into the
> subagent's reply contract. Committing is necessary, not sufficient: the plan
> is not on `main` until it lands there, which is the check immediately below.
>
> Note the detector trap this also fixes. **This paragraph is background for
> whoever builds the corpus-wide sweep — it is not a step to run here.** A
> ref-based sweep finds **zero** of these. "Absent from `origin/main`" is not
> "stranded": a plan archived or renamed off main still appears on every branch
> forked before the removal (33 of 34 candidates, a 97 % false-positive rate).
> The committed-ref test is
> `git log origin/main --diff-filter=A --follow -- <path>`, and it cannot see an
> untracked file at all. Catching the untracked ones needs
> `git status --porcelain -uall -- plans/` (a cwd-relative pathspec — run it
> from the repo root) across every worktree, PLUS a sweep of unpushed local
> branches, which are on a ref and so invisible to `git status` while being just
> as invisible to every peer.

**Then land the plan with the helper — the push is not the publication.**
A branch pushed with no pull request, onto a repo that needs one, never reaches
`main`, so the plan stays invisible to every `origin/main` reader (9 stems were
pushed and never proposed on 2026-09-02). Publish the committed plan file with
`bash <workspace-root>/qontinui-claude-config/scripts/land-plan-stamp.sh "<plans-repo-root>" "<repo-relative path>" "<local file>" "<commit subject>"` rather than asserting from prose whether the plans repo needs a PR:
the helper's ruleset probe of the default branch decides. It lands the blob
directly and prints `LANDED <commit|unchanged> <blob>`, or — where a PR is
required or the probe cannot tell — cuts a fresh branch, opens a new PR with
`gh pr create` (the only opener it runs; never `gh pr merge`) and prints
`PROPOSED <pr-url|branch> <branch>`. A caller holding coord's MCP door that
wants `coord_create_pr` sets `LAND_PLAN_STAMP_NO_PR=1` — the helper then pushes
and reads back the branch, prints it, and opens nothing — and opens the PR
itself with `coord_create_pr`, falling back to `gh pr create`. It never pushes to an existing branch. A
non-zero exit means the plan is NOT published — report it. On `PROPOSED`, read
`gh pr list --repo <owner/repo> --head <branch> --state all --json
number,state,headRefOid` for the branch the line names. A NON-stamp push to an
existing branch stays governed by
`knowledge-base/qontinui-specific/coord-ff-lands.md` → "Pushing to a branch
whose PR may already have landed". Runbook:
`knowledge-base/qontinui-specific/bodyless-work-units-and-stranded-plans.md`.

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
- **When the argument was a dossier**: name it — slug and the head id Step 0
  resolved — and **record the remediation back onto it** before reporting,
  so the dossier learns about its own plan rather than waiting for a closeout
  to notice. Re-read the head first (a peer may have superseded it since Step
  0), then post a superseding head:
  `coord_post_finding(kind="dossier", topic="dossier:<slug>",
  supersedes=<current head finding_id>, …)` carrying the head's body and
  `artifact_refs` forward unchanged except that `remediations` gains
  `{"plan": "<new plan stem>", "status": "DRAFT", "held": false}` (and
  `readiness` moves to `in_remediation` if it read `ready_for_pvi`). Report the
  new `finding_id`. On the **kind rejection** — or for a head that is still a
  memory record — record the pointer as a memory record instead, titled
  `DOSSIER-CONTRIB <slug> — remediation plan <new stem> authored (DRAFT)`, and
  say plainly that **the head update is owed**: the next `/unattended` 2b-bis
  that touches the slug merges it forward. Never leave the plan unrecorded
  because the primary door refused — an unrecorded remediation is how a
  dossier comes to list five shipped fixes it never knew about.

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
