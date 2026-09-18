# Research Plan

Produce a **research prompt** for a cheaper/faster model (DeepSeek, Haiku, or
whatever the operator is routing to — never assume a specific one) to execute
in a separate session, so the exhaustive fact-gathering step for an upcoming
plan doesn't burn expensive-model tokens on mechanical grep/read work. This
command does **not** investigate the codebase itself beyond light
reconnaissance, does **not** write a plan, and does **not** call the target
model — it writes one file: the prompt.

This is the optional stage that sits **before** `/create-plan`. Normally
`/create-plan` Step 3 does all its own research; when you've run
`/research-plan` first, hand the cheap model's raw output back to
`/create-plan` alongside the topic (see Step 6) so it verifies and
synthesizes pre-gathered evidence instead of re-discovering everything from
scratch.

## Arguments

- `$ARGUMENTS` — same three-way resolution as `/create-plan`:
  - A **path** to a prompt file (e.g. `<prompts-dir>/foo.md`).
    If it exists, `Read` it in full — its content is the topic.
  - **Inline text** — a problem description, bug report, or "investigate X"
    ask, typed directly. Used verbatim.
  - **Empty.** Glob the prompts directory beside the plans directory
    (`$QONTINUI_PLANS_DIR/../prompts/*.md`; see
    [Plan directories](#plan-directories)), sort by mtime, confirm the most
    recently modified candidate with the user before proceeding. If no such
    directory exists, ask the user for the topic rather than guessing one.

## Plan directories

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
  command writes its research-prompt and findings files into. The qontinui runner
  injects it into agent sessions from its `paths.plans_dir` setting; a session
  launched outside the runner will not have it. **If it is unset, ask the user once
  where plans live, or DISCOVER one: from the workspace root,
  `ls -d plans */plans 2>/dev/null` and use the directory that actually exists** — say
  which, and ask when it finds none or more than one. Never fall back to a directory
  you have not confirmed is there; a named fallback fails silently on every machine
  that does not have it. Never assume an absolute path from
  another machine.

Expand these to real absolute paths before writing any file or embedding a path in a
composed prompt: the composed prompt runs in another model's session, whose
environment will not have these variables.

## Instructions

### 1. Resolve the topic

Same as `/create-plan` Step 1. Hold the resolved topic text; everything below
is grounded in it.

### 2. Check for existing coverage (light — not the exhaustive check `/create-plan` itself does later)

- **The corpus** — the authoritative surface per the block above. With a candidate
  stem, `GET <web-origin>/api/v1/plan-library?kind=plan&work_unit_slug=<stem>`;
  otherwise page `?kind=plan&limit=200` and match `slug`/title yourself. **Never
  probe by stem with `?q=`** — it matches title and body, not the slug.
- `Glob` `$QONTINUI_PLANS_DIR/*.md` for a title/slug that plausibly already
  covers this topic (grep filenames/titles for its key nouns). Skipped, not
  failed, when the variable is unset.
- One `git log --all --oneline -i --grep` pass + one `gh pr list --state all
  --search` pass on the topic's key terms.

If any of them turns up a clear hit (an existing plan, or a merged PR that already
did this), surface it and confirm with the user whether to proceed anyway
(the topic may only partially overlap) before spending a research prompt on
work that's already done.

**An empty sweep is not a licence to spend the prompt.** A zero-result corpus read
is UNKNOWN whenever the body sync is unconfirmed, and the other two passes only
ever see what someone committed or opened a PR for. When the corpus half came back
UNKNOWN, say which checks actually ran before proceeding — do not report "no prior
coverage" as though the authoritative surface had answered.

### 3. Verify the checkout isn't stale — mandatory, not optional

Before treating anything in the repo as "current state," for every repo in
scope: `git fetch origin`, then `git rev-list --count HEAD..origin/<default
branch>` and `git status -sb`. A nonzero count, or a current branch that
isn't the default branch, means the working tree is stale/off-branch — grep
output from it is **not** current state and must not be presented as such,
either to yourself in Step 4 or to the target model via Step 5's prompt.

This is not a hypothetical: it has already caused a real miss. A prior
`/research-plan` run investigated a "bug" that was fully fixed 5 days
earlier by a coord-orchestrated rebase-land (PR closed, not merged, per
coord's normal land mechanics — see the topic this command exists partly to
help investigate) — because the local checkout being grepped was 284 commits
behind `origin/main` on an unrelated branch, and nobody checked. The
composing session and the target model can each independently be stale; a
clean check here doesn't guarantee the target model's environment is
clean too — which is why Step 4 defaults every source-reading task to
`origin/<default-branch>` content rather than the bare working tree (see
below), so staleness on either side stops being able to produce a silent
wrong "current state" claim.

If the checkout is stale/off-branch, don't attempt to fix it (not this
command's job) — just make sure every task you compose in Step 4 reads
`origin/<default-branch>` content explicitly, never the bare working tree.

### 4. Light reconnaissance — enough to target the cheap model, not enough to answer the question yourself

Budget: roughly 6-8 tool calls. Enough to:

- Identify the repo(s) in scope.
- Confirm the file(s)/subsystem the topic names actually exist at their
  named location (or find where they really live, if the topic's naming is
  approximate) — **read/grep against `origin/<default-branch>` content per
  Step 3**, not the bare working tree, if Step 3 found the checkout stale.
- Pull 3-8 concrete **literal anchor strings** — exact function/struct/enum
  names, error-message substrings, existing memory-file names, a plan stem —
  that Step 5's task blocks can grep for verbatim.

**Stop there.** If you catch yourself reading a full function body to
understand *behavior* or *control flow*, you've crossed into the job you're
about to hand off. Confirm existence and collect search terms; don't reason
about what the code does yet — that reasoning is what the eventual plan-writing
session (informed by the cheap model's raw findings) does, not this step.

### 5. Compose the research prompt

The target model is assumed weaker and cheaper than you, with two known
failure modes this contract exists to defend against: **inventing
plausible-but-wrong conclusions**, and **silently skipping a search that
returned nothing** (indistinguishable, downstream, from "didn't check"). The
composed prompt MUST enforce:

- **The deliverable is a FILE, not chat output.** The composed prompt must
  instruct the target model to write its complete raw output, in the task-block
  shape below, to a specific path you dictate:
  `<resolved $QONTINUI_PLANS_DIR>/<slug>-research-findings.md` — write the **expanded
  absolute** path into the composed prompt, never the variable name; the target model
  runs elsewhere and will not have it set. (Same directory and
  slug as the research-prompt file this command writes in Step 6, so the two
  sit side by side and the Claude session in Step 6's handoff can `Read` the
  findings file directly instead of the operator copy-pasting chat output
  between sessions). Tell it explicitly: use its write/edit tool if it has
  one, creating the file fresh (overwrite if present); if — and only if — it
  has no file-write capability at all, fall back to printing the task blocks
  verbatim in its response instead, clearly labeled so the operator knows to
  save it by hand. State both branches in the composed prompt; don't assume
  which one applies.
- **Default every source-reading command to `origin/<default-branch>`
  content, not the bare working tree** (see Step 3): `git show
  origin/<branch>:<path> | grep -n ...` / `sed -n` on that output, or `git
  --no-pager grep -n <pattern> origin/<branch> -- <path>`, instead of bare
  `grep -rn <pattern> <dir>`. The target model's own checkout can be stale or
  on the wrong branch exactly like yours might be in Step 3, and you have no
  way to verify that from here — reading `origin/<branch>` content directly
  makes the findings correct regardless of what branch/staleness state the
  target model's working directory happens to be in. State the default
  branch name explicitly in the prompt (don't assume `main`).
- **Output shape — TASK blocks, nothing else (whether written to the file or
  printed as the fallback):**
  ```
  == TASK <letter>: <one-line label> ==
  $ <exact literal shell command>
  <raw output, verbatim>
  ```
- **Every command is a real, portable shell command** — `grep -n`, `git log`,
  `find`, `cat`, `rg` — **never** a reference to an internal tool name like
  `Grep`/`Read`/`Glob`. Those are this session's tool names; the target
  model's harness is unknown and may only have a plain shell.
- **Explicit negative-result markers, always emitted, never silently
  skipped:** `NO MATCHES` for an empty grep, `FILE NOT FOUND: <path>` for a
  missing file — a task that finds nothing still produces its block. The
  same honesty applies at the *over*-abundant end (see the match-count cap
  below): truncation must be a marked fact, never a silent trim.
- **Capped context per match** (e.g. `-B2 -A15` on grep) — no full-file
  dumps. For a bounded line range, the command must be a real, literal shell
  command — `sed -n '<start>,<end>p' <file>` (or `awk 'NR==<start>,NR==<end>'`)
  — **never** a pseudo-command like `$ read <file> <range>` (not a real
  shell command; a weaker model will fabricate this exact non-command when it
  means "I used my file-read tool," breaking the "every command is real and
  portable" rule above).
- **Capped match COUNT, not just per-match context.** A grep for a common
  struct field, generic string literal, or short identifier can return
  hundreds of matches that are mostly boilerplate (SQL column lists, test
  fixture inits) — this bloats the findings file and burns the plan-writing
  session's context reading it, for little marginal signal. Any task whose
  pattern is likely to be broad must pipe through a count cap (e.g.
  `| head -n 40`) and, if the true count exceeds the cap, say so explicitly:
  `... TRUNCATED — showing 40 of <N> total matches (run 'grep -c ... ' for
  the exact count if needed)`. Prefer narrowing the pattern instead of
  capping where possible — see the task-composition guidance below.
- **Hard DO-NOT list, stated explicitly in the prompt:** no summarizing, no
  "this suggests," no conclusions, no proposed fix, no plan skeleton, no
  confidence claims. Raw facts only.
- **A one-line header before Task A**: topic (one line), repo(s)/cwd every
  command in the prompt assumes, and today's date — so a reader opening the
  findings file cold (possibly a different Claude session than the one that
  wrote the prompt) doesn't have to infer what repo the bare relative paths
  in every task block are rooted at.
- **No preamble beyond that header, no closing summary** — output starts at
  the header, then Task A's block, and ends at the last task's block.

Populate the task list from what Step 2/3/4 found — every task should trace
back to something the topic named or your recon surfaced, not padding:

1. **Refresh tasks** — one per concrete claim/example the topic names (a
   function, a bug report, a prior finding). Grep for the **identifier
   broadly across the repo** (e.g. `git --no-pager grep -n "pr_merged"
   origin/main`), not a narrow "does this one cited line still say X" check
   — a broad identifier grep costs the same one task but, empirically, tends
   to surface neighboring prior art (sibling functions, doc comments
   explaining the whole subsystem, existing fixes) for free. A
   narrowly-scoped single-line refresh only confirms drift; it doesn't
   discover anything.
2. **Generalization sweep** — the topic's named example(s) are instance(s)
   of a *pattern*; task(s) that grep for the generalized shape (not just the
   specific examples) to hunt for **more** instances of the same class
   across the repo(s) in scope.
3. **Prior-art / existing-primitive check** — literal greps for a
   shared helper, resolver, or abstraction that might already solve this or
   that ought to be the single source of truth every consumer calls (mirrors
   `/create-plan`'s prior-art search, but expressed as mechanical greps a
   weak model can execute rather than open-ended reasoning).
4. **Duplicate-work sweep** — an exhaustive, multi-keyword version of Step
   2's light check: `git log --all --grep`, `gh pr list --state all
   --search`, and a filename grep over the plan directories (name them by
   resolved absolute path in the composed prompt), each
   run with several keyword variants (the cheap model can afford the
   thoroughness you didn't spend time on in Step 2).
   **Scope this honestly in the composed prompt**: the target session holds no
   coord credential, so it cannot read the authoritative corpus — this sweep is
   filesystem-and-git only, and an empty result from it is UNKNOWN, not absence.
   The corpus check is Step 2's, and it stays in *this* session.
5. **Best-effort internal-tool task** (only if relevant) — e.g. a coord-mcp
   finding-history query — explicitly marked `SKIPPED — no <X> access` as
   the required output if the target session lacks that tool, per the
   negative-result rule above.

**Prefer identifier/function/type-name patterns over generic field-name or
short-string sweeps.** A specific symbol (`pr_merged_verdict`,
`classify_clone_stderr`) greps tight and high-signal even run broadly. A
generic struct field or common short string (`merge_state_status`, `"merged"`)
matches everywhere the type is merely *touched* — hundreds of SQL column
lists and test-fixture inits, almost none of it insight — and needs the
match-count cap above even so. When the topic genuinely requires "every
consumer of this data field," say so and accept the capped/truncated result;
don't default to a field-name sweep when an identifier sweep would answer
the same question tighter.

Don't invent tasks beyond what's traceable to the topic or your recon — a
bloated task list wastes the cheap model's context exactly the way it would
waste yours.

### 6. Save and report

- Get today's date from the shell (`date +%F` — never guess). Derive
  `<slug>` the same kebab-case way `/create-plan` would name the eventual
  plan, so the two files pair up visibly.
- Write the composed prompt to
  `$QONTINUI_PLANS_DIR/<YYYY-MM-DD>-<slug>-research-prompt.md` via
  `Write`, and report the resolved absolute path (not the variable).
- **Author it in a worktree — never the primary/shared checkout — and commit +
  push it at creation**, rather than leaving it untracked for whoever opens that
  directory next. Same rule the plan itself follows: `/create-plan` commits the
  plan at creation stamped `DRAFT`, because an untracked plan is invisible to
  coord's `conflict_check` and unreadable by the non-owner session that must vet
  it (`vetted` is attested; self-attestation is rejected). Skip only if the
  plans directory is not a git repo — and PROBE that
  (`git -C <dir> rev-parse --show-toplevel`) rather than assuming it, since pointing
  `$QONTINUI_PLANS_DIR` at `qontinui-dev-notes`'s `plans/` directory is a supported
  configuration.
- **Then assert a PR carries that branch's push — the push is not the
  publication.** On a coord-merge-authority repo a pushed branch with no pull
  request never reaches `main`, so the prompt is on `origin` and invisible to
  every `origin/main` reader (9 plan stems were pushed and never proposed on
  2026-09-02). Read `gh pr list --repo <owner/repo> --head <branch> --state all
  --json number,state,headRefOid` and apply
  `knowledge-base/qontinui-specific/coord-ff-lands.md` → "Pushing to a branch
  whose PR may already have landed". Use `--state all` because an empty
  open-only answer cannot tell never-proposed from closed-under-you, and a
  CLOSED or MERGED PR carries nothing pushed after its close. When commits
  remain unlanded, take its fresh-branch path and open the new PR,
  `coord_create_pr` first, then `gh pr create`; **never `gh pr merge`, never
  `--admin`**. Runbook:
  `knowledge-base/qontinui-specific/bodyless-work-units-and-stranded-plans.md`.
- Also print the full prompt content in your response, so the operator can
  copy-paste it without opening the file.
- Report, under 80 words: the file path, the repo(s) in scope, the task
  count, and the handoff instruction: *"Run this against the cheap model —
  it's instructed to write its findings to
  `<slug>-research-findings.md` next to this prompt itself, so once it's
  done, run `/create-plan <path-to-topic>` and mention the findings file so
  Claude reads it as pre-gathered evidence instead of re-discovering
  everything. If the target model has no file-write tool, it'll print the
  task blocks instead — save that output to the same path by hand first."*

## Rules

- **One new file.** This command writes the research prompt and nothing
  else — no plan, no code, no findings file (that comes from running the
  prompt against the target model, a separate step the operator does).
- **The recon budget in Step 4 is real.** If it balloons past a handful of
  calls, you're doing the cheap model's job for it. Compose the prompt with
  what you have — imperfect task-scoping is something the target model's own
  `NO MATCHES` markers will surface, not something to perfect here.
- **The staleness check in Step 3 is not skippable.** It already caused one
  real false-positive investigation (see Step 3) — a `git fetch` + two
  comparisons is cheap; a plan written around an already-fixed bug is not.
- **Every composed command must be plain-POSIX-shell runnable** — assume
  nothing about the target session's tool surface beyond a shell and this
  repo checked out at a known path.
- **Never name a specific target model as a hard requirement** in the
  composed prompt's instructions — say "you are doing fact-gathering," not
  "you are DeepSeek." The operator may route this at any model.
- **Don't call the target model yourself and don't write the plan.** Both
  are separate, later steps the operator or `/create-plan` owns.
