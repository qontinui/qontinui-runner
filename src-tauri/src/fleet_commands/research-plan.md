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

- **`$QONTINUI_PLANS_DIR`** — the directory plans live in, and the directory this
  command writes its research-prompt and findings files into. The qontinui runner
  injects it into agent sessions from its `paths.plans_dir` setting; a session
  launched outside the runner will not have it. **If it is unset, ask the user once
  where plans live, or fall back to `<workspace-root>/plans`** (a `plans/` directory
  beside the repos this session is working in). Never assume an absolute path from
  another machine.
- **`$QONTINUI_PLANS_ARCHIVE_DIR`** — optional, normally unset. When set and different
  from `$QONTINUI_PLANS_DIR`, it holds already-archived plans; include it in Step 2's
  and Step 5's existing-coverage sweeps — an archived plan still counts as coverage.

Expand these to real absolute paths before writing any file or embedding a path in a
composed prompt: the composed prompt runs in another model's session, whose
environment will not have these variables.

## Instructions

### 1. Resolve the topic

Same as `/create-plan` Step 1. Hold the resolved topic text; everything below
is grounded in it.

### 2. Check for existing coverage (light — not the exhaustive check `/create-plan` itself does later)

- `Glob` `$QONTINUI_PLANS_DIR/*.md` — plus `$QONTINUI_PLANS_ARCHIVE_DIR/*.md` if that
  variable is set and different — for a title/slug that
  plausibly already covers this topic (grep filenames/titles for its key
  nouns).
- One `git log --all --oneline -i --grep` pass + one `gh pr list --state all
  --search` pass on the topic's key terms.

If either turns up a clear hit (an existing plan, or a merged PR that already
did this), surface it and confirm with the user whether to proceed anyway
(the topic may only partially overlap) before spending a research prompt on
work that's already done.

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
  plans directory is not a git repo.
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
