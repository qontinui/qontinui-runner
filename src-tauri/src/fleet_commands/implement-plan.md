# Implement Plan

Execute an approved implementation plan end-to-end in a single session, without stopping between phases.

**Prerequisites:** A plan must already exist and be approved in the current conversation.

## Arguments
- `$ARGUMENTS` - Optional: specific notes or constraints for this implementation run

## Plan directories

Every plan path below resolves from one environment variable. The qontinui runner
injects it into agent sessions from its `paths.plans_dir` setting; a session
launched outside the runner will not have it.

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
>      invisible here), exact stem match, `origin/main` as of the last fetch.
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
>   that observation. Never `q=<stem>`: it matches title and body, not the slug.
> * **A zero is UNKNOWN until a count says otherwise.** The body sync that fills
>   `agent.work_artifacts` is a property of each writing device's runner build
>   (opt-in under `QONTINUI_PLAN_LIBRARY_SYNC=1` before plan
>   `2026-09-03-plan-library-write-door-nonce-authorized-and-body-sync-on-by-default`
>   Phase 3, on by default after it) and is gated per cycle on the tenant's
>   `plan_capture` dial, so a `200` carrying an empty list from a frozen corpus
>   is byte-identical to "no such plan". Record `corpus_health` (or the two
>   counts) beside the zero, and never write a cause for a door that did not
>   answer — settle one against a second independent instance first.
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
  that does not have it. Never assume an absolute path
  from another machine, and never write a plan somewhere you had to guess.
- **Suite directories** — a multi-plan suite lives in its own directory *beside*
  `$QONTINUI_PLANS_DIR` (`$QONTINUI_PLANS_DIR/../<plan-dir>/`), optionally carrying an
  `00-index.md`.

**Neither directory has to be inside a git repo.** Wherever this skill commits or pushes
a plan edit it first checks `git -C "<dir>" rev-parse --is-inside-work-tree`; when that
fails, the edit on disk is the whole ritual. Nothing here requires a second repo.

## Instructions

This skill orchestrates the full implementation workflow. **Phases run as subagents to save context.** The main conversation tracks progress and coordinates — heavy work happens in agents.

### Step 0: Create Phase Checklist

Create a task checklist so progress is tracked. Use TaskCreate for one task per phase, plus tasks for manual testing, spec updates, and commit as applicable. Mark each task complete (TaskUpdate) immediately when done.

### Step 0.3: Name this session + terminal after the plan

Derive a human name from the plan filename and use it to title the session
and the terminal window. The rule: take the plan **stem** (filename without
`.md` or directory — the same `<plan-stem>` Step 0.6 computes), strip a
leading `YYYY-MM-DD-` date prefix, and strip a trailing ` plan` / `-plan` /
`_plan` word if present.
(e.g. `2026-05-21-coordination-improvements` → `coordination-improvements`;
`2026-06-02-fleet-auth-plan` → `fleet-auth`.)

Run this once, substituting `<plan-stem>`:

```bash
DISPLAY=$(printf '%s' "<plan-stem>" \
  | sed -E 's/^[0-9]{4}-[0-9]{2}-[0-9]{2}-//; s/[-_ ][Pp][Ll][Aa][Nn]$//')
mkdir -p "$HOME/.qontinui/session-titles"
printf '%s\n' "$DISPLAY" > "$HOME/.qontinui/session-titles/$CLAUDE_CODE_SESSION_ID"
echo "$DISPLAY"
```

**Terminal title:** automatic. Writing the title-hint file is all you need —
the `set-terminal-title.sh` Stop hook reads it and titles the terminal when
this turn ends. Do NOT try to `echo` an escape sequence yourself; the harness
strips tool-stdout control bytes, so it would silently no-op.

**Session label (`/rename`):** this is the one thing a command cannot set on
its own — Claude Code only renames the session when the operator types
`/rename`, never programmatically mid-turn. So surface the derived name to the
operator as a ready-to-paste line, e.g.:

> Session named `coordination-improvements`. To set the Claude session label,
> paste: `/rename coordination-improvements`

The paste only affects the in-app session label; the terminal title is already
handled by the title-hint file above.

This step is best-effort: if `$CLAUDE_CODE_SESSION_ID` is unset or the write
fails, skip it silently and continue — naming never blocks implementation.

### Step 0.4: Verify dependencies satisfied

Before stamping the plan IN PROGRESS, check whether the plan declares any
upstream dependencies and confirm they're satisfied. This gate exists so
that a plan whose upstream prerequisites haven't shipped doesn't silently
get implemented against a half-built substrate.

#### Read the plan's `Depends-On:` field

A plan MAY declare upstream dependencies inline in its status blockquote
using a `Depends-On:` suffix:

```markdown
> **Status: VETTED 2026-05-21.** <summary>. Depends-On: 2026-05-20-default-tenant-propagation, 2026-05-19-some-other-plan.
```

Parser rule (kept consistent with `/vet-plan` and `/verify-plan-status`):

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

**Canonical resolver (preferred):** the procedure above is implemented in
`qontinui-stack/scripts/resolve-plan-deps.py`. When the stack repo is
available, shell out to the helper instead of re-implementing the parse
inline:

```bash
python <workspace-root>/qontinui-stack/scripts/resolve-plan-deps.py \
    "$QONTINUI_PLANS_DIR/<this-plan>.md" --json
```

It emits `{plan_stem, depends_on[{stem, status, location, summary}],
all_satisfied, unsatisfied[{stem, reason}]}` and exits 0 / 1 / 2 (all
satisfied / some unsatisfied / input error). The `reason` field
distinguishes `missing_file`, `not_yet_shipped`, and `terminal_blocker`,
which maps directly onto the gate behavior below. Fall back to the inline
procedure when the script isn't checked out (containers, CI without the
stack repo, etc.) — both paths are kept in sync as Phase 3.2 of
`2026-05-21-coordination-improvements`.

If the plan has no `Depends-On:` field, skip this step silently and proceed
to Step 0.5. **This is the common case.**

#### Look up each dep's status

For each dep stem, resolve to a plan file:

1. Try `$QONTINUI_PLANS_DIR/<stem>.md`.
2. If that doesn't exist, check the suite dirs beside it (`$QONTINUI_PLANS_DIR/../<plan-dir>/`).
3. If still unresolved, the dep is **missing** — abort (see below).

Use `Read` (a failure is the not-found signal) or `Glob` against the
absolute path. Once located, read the dep file's status blockquote and
parse the lifecycle word — one of `DRAFT`, `VETTED`, `IN PROGRESS`,
`SHIPPED`, `PARTIAL`, `NOT STARTED`, `SUPERSEDED`, `OBSOLETE`. A plan with
no status blockquote at all is treated as `DRAFT`.

#### Gate behavior

After resolving every dep, apply these rules:

- **All deps `SHIPPED`** → proceed silently to Step 0.5. No prompt.
- **Any dep in `DRAFT` / `VETTED` / `IN PROGRESS` / `NOT STARTED` / `PARTIAL`**
  → surface a conflict via `AskUserQuestion` (see below). Do NOT stamp
  IN PROGRESS until the operator resolves.
- **Any dep file is missing** (no searched directory has `<stem>.md`)
  → **abort the skill** with an actionable error before stamping anything.
  Example error text (print the directories you actually resolved, not the
  variable names):
  ```
  Cannot start implementation: plan declares Depends-On: <stem>, but no
  matching plan file exists at:
    $QONTINUI_PLANS_DIR/<stem>.md
    $QONTINUI_PLANS_DIR/../<plan-dir>/NN-<stem>.md

  Fix the Depends-On stem in the plan's status block (typo? renamed
  upstream?) or remove the entry if the dep no longer applies, then
  re-run /implement-plan.
  ```
  Do not auto-correct or guess at the intended stem — that's the
  operator's call. An abort here is correct behavior: a Depends-On that
  references nothing is a broken graph edge.
- **Any dep stamped `SUPERSEDED` or `OBSOLETE`** → surface via
  `AskUserQuestion` the same as the unfinished-dep case. The dep being
  terminally closed doesn't necessarily mean the current plan should
  proceed (its premise may have moved); the operator picks.

#### Conflict prompt (`AskUserQuestion`)

Header: `Dependency not satisfied`

Question body: list each unresolved dep with its stem, current status,
and resolved location, e.g.:

```
This plan declares Depends-On dependencies that aren't fully shipped:

  - 2026-05-20-default-tenant-propagation — IN PROGRESS (plans/)
  - 2026-05-19-some-other-plan — VETTED (plans/)

How do you want to proceed?
```

Options:

- **Abort** — stop the skill. Releases any coord claims this session
  already acquired (per Step 0.6 try/finally semantics) and exits without
  stamping the plan or launching phase agents.
- **Override-and-proceed** — operator accepts the risk; continue to
  Step 0.5. Capture the override decision in the IN PROGRESS stamp's
  body (e.g., `History: Started despite unresolved deps — operator
  override 2026-05-21.`) so future verifiers see the trail.
- **Pause-until-resolved** — stop the skill *without* aborting the
  broader chain. Emit a single-line note to the operator that the plan
  will need to be re-driven once the upstream lands, then exit. Do not
  stamp the plan.

#### Why this gate exists

`Depends-On:` is an explicit edge in the plan graph — authored, not
inferred. The gate is the read-side enforcement of that edge: when the
graph says "plan A depends on plan B," `/implement-plan` MUST NOT silently
proceed on A while B is still open. The three-way choice (abort / override
/ pause) gives the operator control without forcing a hard block.

This step runs **before** the IN PROGRESS stamp so an aborted run leaves
no trail in the plan file — concurrent agents and future operators see a
clean VETTED plan, not a half-stamped one.

### Step 0.45: Concurrent-work reconnaissance (cheap process guard)

The coord phase claim (Step 0.6) catches another session that is
**live-acquiring the same plan+phase right now**. It does NOT catch work
that already **merged** — a claim is released on completion, so a peer that
finished the same plan an hour ago leaves no live claim, and you'd
re-implement a superset that's already on `main`
([[feedback_check_main_for_concurrent_plan_work]]). This step is the cheap,
no-coord-dependency complement: a 10-second look for already-done or
in-flight work BEFORE you stamp IN PROGRESS.

Do all four (they're fast and independent):

1. **The plan's own status block.** Re-read the top of the plan you're
   about to implement. If it already reads `SHIPPED` / `SUPERSEDED` /
   `OBSOLETE`, STOP and surface to the operator — another run already took
   it (Step 0.5 covers the lifecycle rules, but check here *before*
   stamping so you don't race).

   **`IN PROGRESS` does NOT get the "recent date" qualifier, and this check
   alone is not sufficient for it.** A stale-dated foreign `IN PROGRESS` is
   exactly the lagging stamp that cannot be trusted — a session that
   correctly stopped with a gate watching leaves one *by design*. So on any
   `IN PROGRESS`, regardless of date, apply Step 0.5's disposition (which
   consults coord's derived delivery) rather than deciding here. Note the
   two differ in kind: this step surfaces to the operator with a **Proceed
   anyway** option, whereas Step 0.5's live-peer and unidentified arms are a
   hard STOP. **A Proceed-anyway here does NOT carry past Step 0.5** — it
   releases you from this check only; Step 0.5 still applies its own disposition
   and can still stop the run.

   This check is also `/implement-plan`'s **capture step**. While you are here —
   read-only, and before the reserve — record the status block verbatim (token,
   date, session marker) and run the delivery arm table. Step 0.5 consumes what
   you capture, and this is the last point at which the stamp is guaranteed
   readable.
2. **Merged work on `main`.** For each repo the plan touches, scan recent
   history for the plan's stem, session tags, or distinctive symbols:
   ```bash
   git -C <repo> log origin/main -20 --oneline | grep -iE "<plan-stem keywords>"
   ```
   A hit means the plan (or a superset) may already be live — read the
   commit, and if it covers the plan's scope, surface to the operator
   rather than re-implementing.
3. **Open PRs — AND pushed branches that never got one.** Check for an open
   PR implementing the same plan:
   ```bash
   gh pr list --repo <owner/repo> --state open --search "<plan-stem keywords>"
   ```
   An open PR from another session means a live peer — coordinate rather
   than double-build.
4. **Unpublished peer work on disk.** Checks 1-3 all read something a peer
   *published* — a status stamp, a commit on `main`, a PR. A peer who has been
   editing for hours and committed nothing publishes none of them, and so does
   a peer who committed but never pushed. Every one of those reads comes back
   clean while the work is very much held:
   ```bash
   bash <workspace-root>/qontinui-claude-config/scripts/scan-worktree-wip.sh         "<repo>/<path-the-plan-touches>/*" ...
   ```
   Pass the paths the plan's phases will edit. Exit `1` names the checkout, its
   branch and the intersecting files, tagged `WIP` (uncommitted) or `UNPUSHED`
   (committed, never pushed) — treat either as a live peer. Exit `3` is
   **INCOMPLETE**, which is not an all-clear: the scan has a hole in it, so
   close the hole and re-run rather than reading it as clean.

   This is not hypothetical. On 2026-08-19 this exact step caught a peer holding
   ~30 staged files that implemented a plan's Phase 1 — better than the plan
   did — after `coord_who_is_working_on` had returned `verdict: "clear"` twice
   for the same paths and `coord_session_worktrees` returned zero rows. The
   worktree was hand-made, so nothing registered it. See `/preflight` step 4b,
   which runs the same scan one stage earlier, at authoring time.

   **A PR search alone is structurally blind to pushed-but-unproposed work.**
   In the 2026-08-20 three-way collision the peer's branch
   `followup/1026-ci-apt-marker` sat on `origin` with 2 commits and no PR, so
   this probe read clean over the top of the very work it existed to find. The
   rate is not a one-off: **1 of 40** newest branches in `qontinui-web` on
   2026-08-21 and **1 of 40** again on 2026-08-25 against a fully refreshed
   branch set; fleet-wide on 2026-08-25, **6** such branches across 4 repos.
   So also sweep `origin` tips against `origin/main` (measured working
   2026-08-25):
   ```bash
   gh pr list --repo <owner/repo> --state all --limit 400 --json headRefName \
     -q '.[].headRefName' | sort -u > /tmp/prbranches.txt
   git for-each-ref --sort=-committerdate --format='%(refname:short)' refs/remotes/origin \
     | grep -E 'origin/(agent|followup|impl|feat|fix)/' | head -40 | sed 's|^origin/||' \
     | while read -r b; do
         grep -qxF "$b" /tmp/prbranches.txt && continue
         n=$(git rev-list --count origin/main..origin/"$b")
         [ "$n" -gt 0 ] && echo "$b (+$n ahead, no PR)"
       done
   ```
   **`--state all`, not `--state open`, is deliberate.** A branch whose PR was
   CLOSED unmerged HAS been proposed — it is not pushed-no-PR, and scoring it
   as such produces false positives. (This is not hypothetical: the incident
   branch above acquired a closed PR between 2026-08-21 and 2026-08-25, which
   is exactly the transition that flips a branch out of this signal.) Treat a
   returned branch the same as an open PR: read its tip, and if it touches this
   plan's scope, coordinate rather than double-build. **Zero here is only
   informative when the sweep ran** — a failed `gh` call is UNKNOWN, not clean.

If any surface shows the work is already done or in-flight, surface a
one-line summary + the evidence (commit SHA / PR number / branch name + ahead
count / checkout path) via `AskUserQuestion` (header `Already implemented?`,
options: **Abort** / **Proceed anyway** — e.g. the existing work is partial). If
all four are clean, proceed to Step 0.48. This is a read-only reconnaissance — it
never mutates anything and adds ~10s, far cheaper than building a redundant PR
that gets closed.

### Step 0.48: Reserve the PLAN in coord (before the first write)

Everything up to here is read-only. Everything after it writes: the IN PROGRESS
stamp (Step 0.5) is the mutation that advertises *"I own this plan"*, and until
2026-08-25 it landed **one step before** the exclusion primitive that would have
told this session it does not. Two sessions that both pass Step 0.45 both stamp,
and the `held` signal — when it finally fires at Step 0.6 — fires against a file
both have already written. Exclusion must precede mutation. That is why this step
sits here and not after the stamp.

**Two granularities, both needed — do not collapse them:**

| | Key | Means | Who collides on it |
|---|---|---|---|
| **Plan reserve** (this step) | `plan:<plan-stem>` | "this document is mine to move" | a vetter vs. an implementer; two vetters; two implementers before either picks a phase |
| **Phase claim** (Step 0.6) | `plan:<plan-stem>:phase:<n>` | "this phase's agent is mine to spawn" | two implementers on the same phase number |

The phase key is the reserve key with `:phase:<n>` appended — the phase claim is
strictly **nested under** the reserve, never an alternative to it. A vetter and an
implementer editing the same plan file share no phase number, so the Step 0.6 key
could never see them collide; that is the hole this step closes. Measured
2026-08-25 across `qontinui-web` / `qontinui-runner` / `qontinui-coord` /
`qontinui-claude-config`: **14 strict duplicate-PR pairs in 60 days** (one MERGED
+ one CLOSED-unmerged, < 4 h apart, ≥ 2 shared files, Jaccard ≥ 0.50), out of 1588
PRs in window.

This is the same reserve `/preflight` step 0 specifies
(`.claude/skills/preflight/SKILL.md` → "0. Reserve the plan (free today — do this
FIRST)"), on the same key, and `/vet-plan` step 0.2 and `/vet-imp` Step 1.1 issue
the identical call. Wiring all three lifecycle commands to one protocol makes
**`/preflight` load-bearing for the entire plan lifecycle** — a bug in it now
breaks vetting and implementing alike, not one steward loop. That is the accepted
trade: one implementation to keep correct beats four that drift.

#### Identity, session and coord base — resolved ONCE, here

Every later coord call in this run (Steps 0.5, 0.6, 0.6.5, 0.7.5) reuses these
values verbatim. Resolve them once; do not re-derive them per phase.

1. **Plan-stem.** From the plan path (e.g.
   `$QONTINUI_PLANS_DIR/2026-05-18-agent-spawn-coordination.md`), the filename
   without `.md` — `2026-05-18-agent-spawn-coordination`. This is the canonical
   cross-agent key: the same string `coord.plans`, `coord.sessions.plan_slug`,
   `unit_ready` gates, the `Plan: <stem>` PR marker and `Depends-On:` all use.
2. **Machine UUID.** Env `QONTINUI_MACHINE_ID` first. Else read
   `~/.qontinui/machine.json` and parse **`"device_id"`** — the canonical name
   post-unified-devices — falling back to `"machine_id"` if present (the legacy
   shape). DO NOT fabricate a value if neither source supplies one; take the
   skip-and-warn path (Step 0.6, "Skip-and-warn for non-coord environments").

   > **This chain used to read `"machine_id"` only, and that was a third
   > fail-open route.** On a current device `~/.qontinui/machine.json` contains
   > `{"device_id": …, "hostname": …}` and carries **no `machine_id` key at
   > all** — so a perfectly well-registered device fell straight through to
   > skip-and-warn and ran uncoordinated, for a reason with nothing to do with
   > coordination. Step 0.6.5 already parsed the correct key; the claim path did
   > not. Keep both spellings, `device_id` first.

   **The wire field does not follow the local key.** `/claims/acquire`,
   `/claims/heartbeat` and `/claims/release` take **`machine_id`**;
   `POST /coord/status` (Step 0.6.5) takes **`device_id`**. Send the same UUID
   under whichever field the route names, regardless of which local key supplied
   it.
3. **`AGENT_SESSION_ID` (the owner-token discriminator).** Resolve a stable
   per-session UUID ONCE, before the first acquire, and reuse it for every
   acquire / heartbeat / release in this run:

   ```bash
   AGENT_SESSION_ID="${QONTINUI_AGENT_SESSION_ID:-${CLAUDE_CODE_SESSION_ID:-}}"
   ```

   `CLAUDE_CODE_SESSION_ID` is the harness session UUID — per-session-unique and
   inherited by spawned phase agents (they run in the same Claude session), so a
   child agent's heartbeat owner-token matches the parent's acquire
   automatically. If both are empty (older Claude Code), omit the field — coord's
   `None` fallback preserves machine-only behavior. (The `coord-curl.sh` wrapper
   injects the same value with the same precedence for any coord call that omits
   it; sending it explicitly keeps the claim path independent of the wrapper.)
4. **Coord HTTP base.** Env `COORD_HTTP_URL` first. Else
   `https://coord.qontinui.io`.

#### The reserve call

Preferred — over MCP, which also scans sibling open PRs for the same slot:

```
coord_reserve_resource(kind="plan", name="<plan-stem>")
```

Fallback that survives a dead MCP transport — and this fallback is the *point*,
not a courtesy. `POST /claims/acquire` is **unauthenticated** (verified
422-on-empty-body 2026-08-21 and again 2026-08-25: *"missing field `kind`"*, not
a 401), while `coord_reserve_resource` has **no HTTP route at all**. In the
session that produced this step the MCP transport was dead and the claim door was
open the whole time:

```bash
curl -fsS --max-time 120 -X POST "$COORD_HTTP_URL/claims/acquire" \
  -H "Content-Type: application/json" \
  -d "$(cat <<EOF
{
  "kind": "semantic_resource",
  "resource_key": "plan:<plan-stem>",
  "machine_id": "<machine_id>",
  "agent_session_id": "$AGENT_SESSION_ID",
  "metadata": {
    "plan": "<absolute-plan-path>",
    "skill": "implement-plan"
  }
}
EOF
)"
```

**The `--max-time 120` is load-bearing, not cosmetic — do not lower it.** This
reserve pays a collision scan on top of the SET-NX, and that scan is *volatile*:
the same call, on the same code, measured **43.8 s cold on 2026-08-26** (47.3 s
also observed) and **7.75 s cold on 2026-08-30**, with warm readings spread
2.4-6.0 s inside a single minute. A budget under the cold cost does not report
"slow" — it reports a **timeout**, and the fail-closed arm below then correctly
reads a perfectly **healthy** coord as an unreachable one. That is not
hypothetical: two runs at a 20 s budget failed exactly that way, which is what
motivated plan
`2026-08-26-mandatory-plan-reserve-cold-cost-trips-its-own-fail-closed-arm`.
120 s is a **floor with headroom, not a target.** Since that plan's Phase 2 moved
the collision scan off the synchronous reserve path, this call is expected to
answer at plain-acquire speed — a scan-free `phase` acquire on this same door
measured **0.33 s** (Step 0.6's phase claim never paid the scan and needs no such
floor). If it ever again takes tens of seconds, the cost has regressed: report
the measured number, do not quietly raise the floor.

**Send the owner token.** `/preflight`'s written HTTP fallback omits both
`machine_id` and `agent_session_id`; this call must carry them. Without
`<machine_id>:<agent_session_id>` a second session **on this same box** silently
takes over the first's reservation — the identical bug that plan
`2026-06-03-coord-session-scoped-claim-owner-plan` (SHIPPED 2026-06-03; coord
PR #271 makes `acquire` SET/compare the owner token and the heartbeat/release Lua
match on it, qontinui-claude-config PR #49 sends it) fixed for phase claims. Not
sending it here reintroduces that bug one key space over. (Omit the
`agent_session_id` line entirely if `$AGENT_SESSION_ID` resolved empty — never
send an empty string.)

#### Branch on the result

- **`granted` / `claimed`** — a fresh reservation. **This run is the OWNER and
  therefore the releaser** (see below). Proceed to Step 0.5.
- **`renewed`**, or a **`held` whose `current_holder_session` equals your own
  `$AGENT_SESSION_ID`** — this session already holds the reserve. This is the
  normal, expected outcome under `/vet-imp`, which reserves at its Step 1.1 and
  then invokes `/vet-plan` and `/implement-plan`, both of which re-issue this
  same call on this same key inside the same harness session. **A re-reserve by
  the same owner token is a renewal, not a conflict** — proceed, and do **not**
  release at the end of this run; the acquirer releases.
- **`held` by a DIFFERENT owner — STOP.** Do not stamp, do not edit the plan, do
  not launch a phase agent. Report the holder, then run **Step 0.6's
  conflict-resolution flow verbatim** (`AskUserQuestion`, header `Claim
  conflict`, options **Abort** / **Wait** / **Steal**) with `kind=semantic_resource`
  and this step's key — one flow, two key spaces, no second copy. When
  `current_holder` equals THIS machine and `current_holder_session` differs from
  `$AGENT_SESSION_ID`, say so explicitly rather than implying a different box:

  ```
  Another session on THIS machine (session <current_holder_session>) already
  holds plan:<plan-stem>.
  ```

  This is the signal that was absent in both documented incidents. It is a STOP,
  not a warning: the whole point of moving the reserve ahead of the stamp is that
  nothing has been written yet, so aborting here leaves no trail.
- **`fork_risk`, or a non-empty `forking_siblings`** — **the reserve no longer
  carries this.** Reserve answers **exclusion only**: `granted` / `claimed`,
  `renewed`, or `held` plus the `holder`. The fleet-wide collision scan that
  produced the fork-risk overlay moved off the synchronous reserve path (plan
  `2026-08-26-mandatory-plan-reserve-cold-cost-trips-its-own-fail-closed-arm`
  Phase 2 — it was 87-96 % of the reserve's cost, and it was already best-effort,
  degraded to an empty sibling list on any error). Do **not** wait for, or branch
  on, an outcome that can no longer fire. A caller that wants fork risk asks for
  it explicitly: **`coord_predict_resource_collisions`** — the same predictor
  reserve used to call, reached directly. Running it is optional here; when you
  do and it names siblings, name them in your first text turn and read them
  before proceeding, exactly as Step 0.7.5 requires for a racing semantic
  resource. If an older coord build still answers `fork_risk` or a non-empty
  `forking_siblings`, read it as the advisory overlay it always was — name the
  siblings — never as a hard holder.
- **`topic_conflict` / `topic_unknown` / `invalid_topic`** — surface verbatim and
  abort. Not expected from this call shape; handle defensively.

#### When the reserve cannot be ANSWERED — fail CLOSED

**First, separate a client budget from an outage — the preferred arm's timeout is
not settable from this file.** `coord_reserve_resource` runs on the MCP
**client's** budget; there is no `--max-time` to write here, so the only thing
this step can do is make that failure *recognisable*. A `coord_reserve_resource`
failure that arrives **faster than the `--max-time` floor above** is a suspected
**client-side budget**, NOT evidence that coord is down. The documented next move
is to re-issue the reserve over the `/claims/acquire` fallback **with the
explicit `--max-time`** — and that retry happens **BEFORE** the verdict below,
never after it. The timed fallback is the cheap disambiguator between *slow* and
*gone*, and it is the one arm whose budget this file actually controls. Only when
the **explicitly-timed** fallback ALSO fails is the arm below reached:

> **Coord unreachable** (connection error, timeout, non-2xx, unparseable body)
> on a device that DID resolve a machine UUID: this is **UNKNOWN, not free.** Do
> not stamp and do not launch any phase agent. Report the transport failure
> verbatim, run `bash .claude/skills/coord-revive/coord-revive.sh --floor-claim` from the real cwd,
> paste its `FLOOR-CLAIM:` block verbatim, and re-issue over the door it
> reports LIVE. "No door is live" is licensed ONLY by that block reading
> `verdict=FLOOR`; then surface to the operator via `AskUserQuestion`
> (**Abort** / **Proceed uncoordinated**) — proceeding is a decision someone
> makes, never a default reached by falling through an undocumented branch.
> A `verdict=UNKNOWN` (exit 5: sampled under this box's own load, or an
> unreadable load) is UNKNOWN — re-run after the builds finish, and never
> write "unavailable" from that sample.
>
> "No door is live" is also exactly as wide as `/coord-revive`'s own AXES table.
> A full sweep ends with
> `axes: hosts=127.0.0.1,coord.qontinui.io,api.qontinui.io prefixes=/coord-mcp,/mcp,/agents/credential,/coord/agent-,/api/v1 credentials=proxy-nonce,acting-bearer,device-jwt,bootstrap-agent-jwt,user-device-jwt unprobed=native-coord-mcp-tools`
> and `VERDICT: DEAD`; an `unprobed=` entry beyond the native-tools row comes
> with `VERDICT: UNKNOWN` and a `FLOOR-CLAIM:` line reading
> `verdict=UNKNOWN … reason=axis-unprobed`, which is **not** "no door" — probe
> that axis before choosing between the two options above.

Step 0.6 states the same arm for the phase claim. It is one rule at two
granularities, not two policies.

#### Keep the reserve ALIVE — start the heartbeat, on `claimed` only

The reserve has a finite TTL and coord evicts it when nothing heartbeats it. A
plan implementation routinely outlives that TTL, so the reserve lapses mid-run
unless something renews it — and the first symptom is a `not_held` at Step 6's
release, hours after the plan actually stopped being protected.

`scripts/coord-claim-heartbeat.sh` owns that loop. Start it **only when this
step returned `claimed` / `granted`** — the acquirer owns the heartbeat exactly
as it owns the release. On `renewed` (the normal outcome under `/vet-imp`, which
reserved at its Step 1.1) do **nothing** here: that chain is already heartbeating
this key, and a second loop only doubles the request rate against it.

```bash
# Ledger path — resolve ONCE here; Step 0.6, Step 1 and Step 6 all reuse it.
CLAIM_LEDGER="$HOME/.qontinui/claim-ledger/${AGENT_SESSION_ID:-nosession}.ledger"

bash <workspace-root>/qontinui-claude-config/scripts/coord-claim-heartbeat.sh add \
  --ledger "$CLAIM_LEDGER" \
  --kind semantic_resource \
  --key "plan:<plan-stem>"
bash <workspace-root>/qontinui-claude-config/scripts/coord-claim-heartbeat.sh start --ledger "$CLAIM_LEDGER"
```

`start` detaches a background loop that re-heartbeats every row in the ledger at
min(row TTL)/3 with a 60 s floor, replaying the owner token
`<machine_id>:<agent_session_id>` on every request — coord matches on that pair,
so a heartbeat without it renews nothing, answers `not_held`, and lets the claim
age out anyway. `status --ledger "$CLAIM_LEDGER"` prints one line per row and a
verdict on its exit code: `LIVE` (0), `STALE` (3), `DEAD` (4), `STOLEN` (5).
Anything but `LIVE` means the claim is **UNKNOWN, not held**.

#### Refresh the agent token — (b) before every phase launch, (c) before every closeout write

A bare agent JWT expires on its own clock, and that clock is shorter than a
multi-phase implementation: the reserve succeeds, phases run for hours, and the
Step 6 closeout writes then 401 against a token that was valid at Step 0.48.
`scripts/coord-agent-refresh.sh` renews it **in place** and mints no new
identity. Run it, with no arguments, at three points in this command: here,
immediately after the reserve; **(b)** immediately before every phase launch
(Step 1); and **(c)** immediately before every Step 6 closeout write (the status
stamp's coord writes, the work-unit transition, gate attestations, findings, and
both claim releases).

```bash
bash <workspace-root>/qontinui-claude-config/scripts/coord-agent-refresh.sh
```

It prints exactly one verdict line. `PROXIED` (the runner refreshes for you),
`FRESH <exp> <ttl>` and `REFRESHED <new_exp>` all exit 0 and mean carry on.
`CREDENTIAL_ONLY <exp>` (exit 6), `EXPIRED <exp>` (exit 7) and
`REFUSED <status> <body>` (exit 8) each mean the next coord write will fail
authentication: report the verdict verbatim and follow the next step the helper
names. **Never** call `POST /agents/allocate` to work around one — that door is
prohibited as a credential rung, and this helper is why nobody needs it.

Every coord bearer this command reads resolves **file first**
(`$HOME/.qontinui/agent-jwt/<AGENT_SESSION_ID>`), then `$COORD_AGENT_JWT` — the
file is what the helper rewrites, so an env-first read would keep sending the
stale token after a successful `REFRESHED`.

#### Release — the acquirer releases, and only the acquirer

In the same try/finally as the phase-claim releases (Step 0.6 → "Claim release on
phase completion") and the final `/coord/status` clear (Step 0.6.5), and on every
abort path including the conflict flow's **Abort**. **Stop the heartbeat FIRST**
— `remove` the row, then `stop` the loop — so the loop cannot re-arm a key this
run is about to give up:

```bash
bash <workspace-root>/qontinui-claude-config/scripts/coord-claim-heartbeat.sh remove \
  --ledger "$CLAIM_LEDGER" --kind semantic_resource --key "plan:<plan-stem>"
bash <workspace-root>/qontinui-claude-config/scripts/coord-claim-heartbeat.sh stop --ledger "$CLAIM_LEDGER"
```

`remove` of the last row stops the loop by itself and `stop` is idempotent
**once the ledger is empty**, so running both in that order is safe. Both are
**skipped on `renewed`, exactly as the release below is** — this run neither
started that loop nor owns it.

⚠️ **`stop` REFUSES (exit 6) while rows remain.** The ledger is keyed on
`$CLAUDE_CODE_SESSION_ID`, which a subagent **inherits** from its parent, so one
file is shared by every context under one harness session and this loop is the
sole renewer of every row in it. A refusal here means the `remove` above did not
run, or a sibling context still holds claims — report which, and do not `--force`
past it. See Step 6 item 7 for the full rule, including why the `renewed` skip
does not cover the subagent case. Then release:

```bash
curl -fsS -X POST "$COORD_HTTP_URL/claims/release" \
  -H "Content-Type: application/json" \
  -d "$(cat <<EOF
{
  "kind": "semantic_resource",
  "resource_key": "plan:<plan-stem>",
  "machine_id": "<machine_id>",
  "agent_session_id": "$AGENT_SESSION_ID"
}
EOF
)"
```

The release MUST carry the SAME `agent_session_id` used at acquire — the owner
token is the match key, so a release that omits it or sends a different session
returns `"not_held"` and leaves the reservation to TTL out. **If this run got
`renewed` rather than a fresh grant, do NOT release**: an outer `/vet-imp` still
holds the chain open, and releasing here would drop the reserve mid-lifecycle.
`"not_held"` is otherwise fine and idempotent.

#### Is a plan reserve MANDATORY? Ask the grammar registry, not this file

Step 0.7.5 fixes the rule: *a resource is mandatory-reserve iff coord has a
**registered `SemanticResource` grammar** for it AND it is NOT land-time
re-pointable*, with the registry as the single source of truth and an explicit
prohibition on hand-maintained lists here. A plan document satisfies the second
half — a plan is never land-time re-pointable.

**The `plan` grammar IS registered** (coord, 2026-08-25 — the sibling half of
this plan's Phase 4). Read the live registry rather than this sentence:

```bash
curl -s "$COORD_HTTP_URL/coord/claims/semantic-resource-grammars"
```

It serves `{class, key_shape, description, land_time_repointable}` plus the rule
text, and `plan` (`plan:<plan-stem>`, not land-time re-pointable) resolves
`mandatory_reserve: true`. The reserve response echoes the same verdict under
`grammar`. So the plan reserve is **mandatory now, by the rule** — not by a list
in this file. If the registry ever stops serving `plan`, the reserve degrades to
advisory automatically and correctly; that is the mechanism working, not a
regression.

> Note the registry did not exist before 2026-08-25. Step 0.7.5 named it as the
> single source of truth while the classes lived only in prose — so "ask the
> registry" was unanswerable, and the honest reading of any earlier
> mandatory-vs-advisory claim in these files is UNKNOWN. It is answerable now.

Mandatory governs whether *skipping* the call is a violation; it softens no
branch above. Always issue the call, always STOP on a foreign `held`, always fail
closed on an unanswerable one. Do not write `plan` into a hand-maintained
mandatory list here — that is what Step 0.7.5 forbids and what the registry
replaces.

### Step 0.5: Stamp the plan as IN PROGRESS

**This edit is the first write of the run, so Step 0.48's plan reserve must
already have returned `granted` / `claimed` / `renewed` before you make it.** A
foreign `held`, or a reserve that could not be answered, stops the run *here* —
before anything has been written — which is the whole reason the reserve moved
ahead of this step.

Edit the plan .md to update its status block to:

```markdown
> **Status: IN PROGRESS <YYYY-MM-DD>.** Implementation started by
> session <short session id or branch name>. Phase tasks: <N>. Started
> from <prior status — usually VETTED <date>>.
```

Rules:
- If the existing block is `Status: VETTED <date>`, replace it with the IN PROGRESS line above and reference the vet date in the body (`Started from VETTED 2026-05-02.`).
- If the existing block is `Status: DRAFT` or absent, add the IN PROGRESS block but warn the user in your first text turn that the plan was not vetted — give them a chance to abort and run `/vet-plan` first.
- If the existing block is `Status: PARTIAL` or `Status: NOT STARTED` (set by `/verify-plan-status`), replace it with the IN PROGRESS block and capture the prior state in the body's `History:` line. Don't run `/vet-plan` first unless the user asks — `/verify-plan-status` doesn't supplant a vet pass, but a recent NOT STARTED is also not a reason to re-vet.
- If the existing block is already `IN PROGRESS`, do NOT simply refresh the date and append your session marker. Apply the disposition in `/vet-plan`'s "`IN PROGRESS` is CONDITIONALLY overwritable" section (keep the two in sync) — including its **unidentified default**: a stamp carrying no session marker, or one you cannot positively attribute to your own current session, is a STOP, not an overwrite. A run that positively identifies the marker as its OWN current session id (a resume, or a Step 0.5 re-run) refreshes rather than takes over. Consult `coord_work_unit_list_citations(<plan-stem>).delivery` FIRST, applying that section's **full arm table in its stated order (4, 3, 2, 1, 5, then 6)**. The capture step here is Step 0.45 check 1, and unlike `/vet-plan` the stamp is still intact at this point — so read it and run the arms inline. In particular: `shipped: true` ∧ `evidence_complete: true` (arm 1) means the work has landed, so **STOP and route to closeout** rather than re-running phase agents against `main`; **the two UNKNOWN arms that a degraded read makes look clean are 2 and 3, and neither is an error shape** — `evidence_complete: false` is **arm 2 regardless of `shipped`** (the two derive independently — `shipped = inputs.delivered`, `evidence_complete = evidence_gaps.is_empty()` — so `shipped: true` ∧ `evidence_complete: false` is reachable, and keying arm 2 on `shipped: false` lets it fall through to the permissive arm), and a top-level `merged_degraded_reason` sitting BESIDE `delivery` is **arm 3**, evaluated ahead of every arm but 4 and **UNKNOWN whatever `delivery` says** — while it is set, every citation's `merged: false` is UNKNOWN rather than an observation. Both answer `200` with a parseable `delivery` and no `citations_error`, so neither is caught by arm 6; and **arm 6 is the DEFAULT** — any error other than `no work-unit with that slug`, any unparseable or non-2xx body, a `citations_error` / `delivery_error` key, an absent `delivery`, or the tool masked / absent / on a dead transport is **UNKNOWN, never "not delivered"**. On UNKNOWN do not treat the delivery read as evidence in either direction: run **`/coord-revive`** if the transport is dead, re-issue over the live door, and otherwise fall through to the stamp arms saying the read was inconclusive. Otherwise, an `IN PROGRESS` stamp carrying a session marker ≠ yours is a **live peer** unless you can positively verify the stamping session is dead with zero work products (transcript tail shows death; worktrees clean and 0 ahead of `origin/main`; no PRs and no branches for the plan). Verified-dead → adopt and append your marker, keeping the trail. Not verified → **STOP**; refreshing the date over a live peer is how PR #479 was built against work PR #468 had already merged.
- If the existing block is `SHIPPED` / `SUPERSEDED` / `OBSOLETE`, STOP — implementing a shipped plan is almost certainly a mistake. Confirm with the user before proceeding.

> **Why the delivery read and not just the token list.** The stamp is an
> authoring-surface artifact that lags by construction: a session that correctly
> stops with a gate watching (coord is the sole merge authority, so it cannot land
> its own last PR) leaves the plan at `IN PROGRESS` *by design*, and that stamp
> stays stale until someone flips it. Meanwhile coord derives
> `work_unit.status = shipped` from merged PR citations and refuses a hand-written
> `shipped` (`422 status_is_derived`), so the derived delivery is the one signal
> that cannot lag. Measured 2026-08-26 on
> `2026-08-20-merge-status-review-required-conflates-two-causes`: five independent
> signals said *shipped* while the plan file said `IN PROGRESS 2026-08-25`.
> Read `verification-and-evidence` `unknown-must-not-render-as-a-default` before
> collapsing any UNKNOWN arm into "not shipped".

**Transition the work-unit registry directly when you stamp IN PROGRESS.** The
IN PROGRESS stamp drives `unit_status` gates, which watch the work unit's `status`
in coord's directly-writable work-unit registry. Set the status
with an explicit transition. **Two agent-side transports, one capability:**
`coord_work_unit_transition` `{slug, to_status}` is the native MCP tool and is the
shorter path when it is visible; `POST
$COORD_HTTP_URL/coord/work-units/<plan-stem>/transition`
`{to_status:"in_progress", by_actor}` is its REST twin (or an upsert carrying a new
`status`). A masked MCP tool is masking, not a closed path. **`shipped` is the
exception — do NOT transition to it by hand (see Step 6):** it is a DERIVED status
coord computes from the work unit's landing predicate, so a direct
`to_status:"shipped"` POST is rejected with `status_is_derived`. The plan `.md`
stamp (in place — Step 6) + any commit/push STAY (the operator-private artifact workflow), but
the coord `in_progress` transition is this explicit call, not a side effect of the
file push. (A repo that is NOT coord sole-authority lands its PRs via normal GitHub flow.)

> ⚠️ **That transition is NOT durable — it can be reverted, and was.** This step
> used to claim "there is no longer a plan-ingest worker mirroring the plan
> directory into the registry, so a direct transition is durable, not reverted by
> an ingest tick." **That is false.** The runner's plan/work-unit adapter
> (`qontinui-runner/src-tauri/src/plan_workunit_adapter/`, `push.rs` →
> `coord.work_units`) reconciles the plans directory into the registry on every
> cycle — measured at ~68 s — whenever a plans dir and a coord base both resolve.
> It writes as actor **`harness-markdown-adapter`** and overwrites the status with
> whatever it reads from the plan `.md` **copy it walks**.
>
> Measured 2026-08-26 on `2026-08-25-coord-console-intent-and-devops-sections` —
> one revert, not a flap:
>
> ```
> (none)      -> draft        by: harness-markdown-adapter   # initial create
> draft       -> in_progress  by: session 0000f4d9 (implement-plan)
> in_progress -> draft        by: harness-markdown-adapter   # the revert
> ```
>
> The adapter was not mis-parsing. It was reading a **different copy** — two stale
> `**Status: DRAFT` copies of the same plan sitting at an older commit in sibling
> worktrees the scanner walks. This is the divergent plan corpus *writing*, not
> merely confusing (CLAUDE.md → "Plan corpus authority").
>
> **What this costs you, precisely.** A `unit_status` gate anchored on
> `in_progress` — the very gate this paragraph exists to drive — **will not clear**
> over a reverted registry. Gates whose predicate does not read unit status
> (`pr_merged`, `commit_live`) are unaffected, and so is `shipped`, which coord
> derives from PR citations independently of the from-status. So a revert delays
> *status-anchored* dispatch and nothing else.
>
> **What to do about it — and what not to.** Read the status back after a full
> adapter cycle rather than assuming the POST held. If it reverted, the cause in
> the one measured case was a stale copy on disk; you can look for one with
> `grep -rl "^# <plan title>" <workspace-root>/*/plans <workspace-root>/**/plans`
> and compare stamps. But **n=1 — do not assume that is always the cause**, and
> note that the copies were in **other sessions' worktrees**, which you must not
> edit (the worktree-claim guard exists for exactly that). If you cannot reach the
> stale copy, that is the expected outcome: **report the residual and carry on.**
> **Do NOT loop re-transitioning — the next scan wins again.**

#### Retire the vet→implement safety net — cancel, then mute (do this AT the stamp)

The IN PROGRESS stamp is the moment this session provably took the work over, so
it is the moment to retire the net that was armed in case it didn't.

Since 2026-07-28 `/vet-plan` §5.4 arms a dispatching `continuation_spawn` under
`/vet-imp` — because the `/vet-imp` chain was observed to stall after vetting,
leaving plans stranded. That continuation exists solely to rescue a chain that
dropped. This session did not drop: it is stamping IN PROGRESS. So retire it, or
the runner will spawn a redundant terminal running `/implement-plan` on the plan
you are already implementing (the pending row survives up to **7 days** — the
window was widened from 24h on 2026-07-23 — and the runner's in-process dedupe
set is forgotten across restarts).

> ⚠️ **The 7 days is the UNDELIVERED-spawn lifetime, not your cancel window, and
> the real window is `deferred_count`-dependent — SECONDS to hours.** Both
> regimes are measured. **Deferred** (gate `d5970373`, 2026-08-22,
> `deferred_reason: "spawn_authorization_deny"`, `deferred_count: 1`): cleared
> 05:12:59Z, dispatched +442 ms, consumed **09:35 later** — a cancel lands
> comfortably. **Un-deferred** (gate `f5940b3f`, 2026-08-30,
> `continuation_deferred_count: 0`): created 21:35:10.527Z → cleared
> 21:35:34.320Z → dispatched 21:35:38.580Z → consumed 21:35:41.499Z,
> `consumed_outcome: "spawned"` — **register → spawned in 30.97 s, and the
> clear→consumption cancel window was 7.18 s.** An agent turn cannot reliably
> fire a write inside ~7 s, so **on an un-deferred gate the cancel will usually
> lose and a redundant terminal WILL spawn.**
>
> Read `continuation_deferred_count` on the gate row to know which regime you are
> in — it is the only tell, and the row read below already returns it. Do not
> promise yourself a comfortable window: fire the cancel anyway (it is race-safe
> and idempotent), **mute regardless**, and if it answers `409 already_consumed`
> report that honestly instead of claiming a clean takeover. This is also why the
> **pre-dispatch** cancel is the one that matters: on an un-deferred gate it is
> very nearly the only reliable window.

**"Born cleared" = the predicate was satisfied at the DOOR — never a row
state.** The row is born `open`, the first sweep tick clears it, and a sibling
gate registered afterwards still pins it back `Open`. It is never terminal.
Canonical: `_gate-registration` → "Registration warnings" → *What "born
cleared" means*.

**The continuation is on the NET gate, not on `unit_ready` (changed 2026-08-30).**
Under `/vet-imp`, §5.4 registers **only** the
`{"kind":"time_elapsed","duration_secs":1800}` **net** gate, under the distinct
`phase_name` `"vet→implement safety net"` — it is the one carrying the
`continuation_spawn`, and since 2026-09-04 it is the **only** gate §5.4 writes on
this arm. (A standalone `/vet-plan` also registers the continuation-less
`unit_ready` **record** gate; under `/vet-imp` that gate is deliberately skipped,
because this step's own transition would strand it — see §5.4's caller table.)
Before the split, the record gate was born cleared (its
`ready_status` equalled the status §5.4 had just transitioned), so the 10 s
`run_gate_sweep` dispatched its continuation within one tick and this step
arrived tens of minutes later to a `409 already_consumed` on **every** completed
run. With the 30-minute net, the expected state here is **pre-dispatch** again.

> ⚠️ **This step's ORDERING is what made the record gate unclearable, and it is
> why that gate is no longer registered on this arm.** The work-unit transition
> above moves the unit to `in_progress` **before** the mute below removes the net
> from `open_siblings` — so a record gate still pinned by that sibling can never
> clear afterwards (`status != ready_status`), fails **OPEN** with no
> `gate_unclearable_terminal` alert, and is reaped by nothing until the 7-day
> stale smell. Measured 2026-09-04: **6 of 25 `unit_ready` gates (24%) ended
> unclearable this way.** Do not "fix" it by moving the mute above the
> transition — that only narrows the race to one sweep tick. The fix is that
> §5.4 does not register the gate under this caller.
>
> ⛔ **And never withdraw a `unit_ready` gate you find stranded.** A `withdrawn`
> row is never `cleared`, while `all_unit_gates_cleared`
> (`work_unit_derive_worker.rs:337-353`) requires `total == cleared` across all
> gates on the unit — so withdrawing one **permanently** prevents that unit from
> deriving `ready`. Agents were already doing this before it was understood.
> Record the `gate_id` and leave the row alone.

**Retiring the net is TWO calls, in this order: cancel, then MUTE.** The cancel is
the race-safe stamp — it forecloses the dispatch. The mute is what unblocks the
record gate: `open_sibling_gates` counts every open gate on the same
`work_unit_id` across phase names, excluding only `unit_ready` predicates and rows
with `muted = true`, and `cancel_continuation` writes only the `continuation_*`
columns — **the verdict is untouched**. So a cancel alone leaves the net gate
`open` forever, and — on a standalone-vetted unit that HAS a record gate —
`unit_ready` reads `Open` with *"…but 1 sibling gate(s) still open"* forever with
it. Mute the **net** gate only — never `unit_ready`, which must stay unmuted to
clear. (Under `/vet-imp` there is no record gate on the unit at all, so the mute
here is purely about not leaving a dead net row open.)

**Branch on the GATE ROW, not on an HTTP status.** Resolve the work unit for
this plan-stem, then `GET $COORD_HTTP_URL/coord/agent-gates?work_unit_id=<id>` and look
at each row's `continuation_spawn` / `continuation_dispatched_at` /
`continuation_consumed_at` / `continuation_cancelled_at` fields. The row to act on
is whichever one carries a `continuation_spawn`. (`/coord/agent-gates`
is the **device-authed** read door — the operator `GET /coord/gates` is
`TenantId`-only and 403s this session's device JWT. **Both writes are
device-authed too**, and each is one capability on two agent-side transports:
`coord_cancel_continuation` / `coord_mute_gate` natively, or the `/agent/` infix
REST twins `POST /coord/gates/<id>/agent/continuation-cancel` and
`POST /coord/gates/<id>/agent/mute` — so this session
does the whole loop itself. This parenthetical used to say "the
cancel below stays operator-only"; that was false, and it is what let gate
`7902e457` spawn a redundant terminal on 2026-08-20.)

| Row state | What it means | What to do |
|---|---|---|
| `continuation_spawn != null` ∧ `dispatched_at == null` | **PRE-DISPATCH — the net is ARMED. This is the EXPECTED state at this stamp**, and what the 30-minute `time_elapsed` net exists to produce. | **Cancel, then mute.** `coord_cancel_continuation` `{gate_id, reason}` (the native MCP tool), or the equivalent REST twin `POST $COORD_HTTP_URL/coord/gates/<gate_id>/agent/continuation-cancel` `{reason}` — legal pre-dispatch, and the point of it: `cancel_continuation` deliberately omits the `continuation_dispatched_at IS NOT NULL` guard (*"the pre-dispatch stamp is the whole point"*). **Then** `POST .../coord/gates/<gate_id>/agent/mute` on the same gate, or the record gate stays pinned `Open` on it as a sibling. `coord_withdraw_gate` is the one-call equivalent and is LIVE. |
| `dispatched_at != null` ∧ `consumed_at == null` ∧ `cancelled_at == null` | Dispatched, not yet consumed — a net older than its 30-minute window, or a gate registered by a coord that predates the split. **Read `continuation_deferred_count` on this row**: `> 0` means a deferral bought you hours; `0` means this window is measured in *seconds* and you are unlikely to still be in it. | **Same two calls, same order, either transport.** `coord_cancel_continuation` `{gate_id, reason}` — the native MCP tool, callable from this device-JWT session — or the REST twin `POST .../coord/gates/<gate_id>/agent/continuation-cancel` `{reason}`. Same capability, same `cancel_continuation_inner`; if the MCP tool is not visible that is masking, not absence, so fall through to the REST door rather than concluding the path is closed. The cancel returns `200 {"cancelled":true}` if you won the race; the mute is required either way. |
| `dispatched_at != null` ∧ `consumed_at != null` | The rescue spawn already fired. The honest fallback arm — reachable on gates registered by an older coord, where the continuation rode the born-cleared `unit_ready` gate. | The cancel answers `409 already_consumed`; **do not** claim a clean takeover (see the responses below). **Still mute the gate.** |
| no row carries a `continuation_spawn` | Genuinely nothing pending — e.g. a standalone `/implement-plan` whose gates were registered continuation-less. | Say so and proceed. **Do not mute anything.** |

**Why the row and not the status code.** The row tells you which gate carries the
continuation and which window it is in; the status code alone conflates a gate
that was never armed with one you failed to reach. Read `continuation_spawn`
first, act second. (This paragraph used to say `continuation-cancel` governs the
"post-dispatch window only" and that a pre-dispatch POST 404s. **That was wrong**
— coord's `cancel_continuation` is deliberately unguarded on
`continuation_dispatched_at`, so the pre-dispatch stamp is a supported call and
the correct FIRST action in every armed row-state. Canonical:
`_gate-registration` → "Continuation cancel + refresh".)

Responses to the cancel:

- **200 `{cancelled:true}`** — the continuation is retired. Now mute the gate.
- **200 `{cancelled:true, already:true}`** — idempotent; it was already
  cancelled (Step 0.6's copy, or an earlier run). Still mute the gate.
- **401 `operator context missing; SSO required`** — **you used the OPERATOR
  route.** This is not a permissions wall and it is not inherent to an agent
  session: it is a wrong door. Both actions have device-authed `/agent/` infix
  twins, which is what this session — holding a device JWT, not an operator
  bearer — must call: `POST .../coord/gates/<gate_id>/agent/continuation-cancel`
  with `{reason}` only (`cancelled_by` derives from the JWT and is not a body
  field), and the mute — `coord_mute_gate` `{gate_id}`, or its twin
  `POST .../coord/gates/<gate_id>/agent/mute`. The unprefixed
  `TenantId` routes are the operator-side equivalents; mention them in a report if
  you like, but never call them from here. Reporting "the net is still armed, a
  redundant terminal may still spawn" on the strength of this 401 is exactly the
  mistake that let gate `7902e457` spawn a redundant session against a SHIPPED
  plan on 2026-08-20. Only if the TWIN also fails is the net genuinely still armed.
- **409 `already_consumed`** — the rescue spawn already fired. Say so honestly:
  a second session may now be working this plan, so reconcile before launching
  phase agents rather than claiming a clean takeover. Mute the gate anyway — the
  spawn is not undoable, but the open sibling still pins the record gate.

**Best-effort throughout — this MUST NOT block the stamp or Step 1.** If none of
it lands, proceed and report the residual honestly — and say which half is
outstanding, since they fail independently: an uncancelled continuation risks a
redundant terminal, while an unmuted net gate pins the `unit_ready` record gate
`Open` and stops the plan publishing as ready, dispatchable work.

**Second line of defence, when the cancel does not land.** How genuinely
*fallback* this is depends on the regime above: on a **deferred** gate the cancel
normally DOES land (`200 {"cancelled":true}`) and this is a true backstop; on an
**un-deferred** gate the 7.18 s window means this path is the expected one, so do
not treat it as rare. A continuation that spawns anyway
runs `/implement-plan` on a plan this session has already stamped IN PROGRESS, so
that run hits Step 0.45's concurrent-work reconnaissance and Step 0.6's
phase-claim conflict and should stand down. That is a real mitigation — but it is
a *behavioural* gate, not a mechanical one, so never treat it as a reason to skip
the cancel, and never report "a redundant terminal may still spawn" without
having tried the `/agent/` twin.

**Run it once per run.** Doing it here means Step 0.6's copy is normally a no-op
second sweep; that is deliberate belt-and-braces, since Step 0.6 is skipped
entirely in non-coord environments and on the claim-conflict path. If you already
cancelled here, Step 0.6's sweep will simply find nothing pending — do not report
it as a second cancellation.

(canonical spec: `_gate-registration` → "Continuation cancel + refresh" — keep in sync)

#### Single-stamp invariant — applies to Step 0.5 and Step 6

A plan must have **exactly one** `> **Status:` blockquote between the H1
and the body. Before writing your stamp:

1. Read the top of the plan. Identify EVERY top-of-file blockquote that
   asserts a status, lifecycle state, or verification date — lines
   starting `> **Status:`, `> **Edit YYYY-MM-DD —`, or `> **Update:`
   all count.
2. Use `Edit` to **delete every existing status-adjacent blockquote** —
   even if a different skill wrote it (`/vet-plan` writes `VETTED`;
   `/verify-plan-status` writes `PARTIAL` / `NOT STARTED`). Yours
   replaces all of them.
3. Then `Edit` again to insert your single new `> **Status:` block.
4. If folding in history is useful (`Started from VETTED 2026-05-02`),
   put it in **one trailing line inside your new block**, prefixed
   `History:`, `Started from:`, or `Previously:`. Never as a sibling
   blockquote.

This stamp is mandatory before Step 1. It makes concurrent agents see
"another session is implementing this" via a quick `head -5 plan.md`
and avoids duplicate work.

### Step 0.6: Coord claim pre-flight (per-phase spawn coordination)

Before launching ANY phase agent in Step 1, acquire a Phase-kind claim from
the coord claims API so a second `/implement-plan` running on the same plan +
phase from another machine (or another shell) sees an immediate structured
conflict signal instead of silently double-spawning. This wires the
`/implement-plan` entrypoint into the L3(b) coordination layer shipped in
the agent-spawn-coordination plan.

This claim is **nested under Step 0.48's plan reserve**, which this run already
holds: the reserve excludes another session from moving the *document*, this
claim excludes another implementer from spawning an agent for the *same phase*.
Both are needed; neither substitutes for the other.

**Skip-and-warn for non-coord environments.** If neither
`QONTINUI_MACHINE_ID` nor `~/.qontinui/machine.json` is available (e.g.
running on a developer laptop that isn't a registered qontinui device),
emit a single-line warning to the user (`⚠️ coord pre-flight skipped: no
machine_id available — running without claim coordination`) and proceed
without claims. This skill MUST remain usable in non-coord environments.

**Why this branch skips and the next one stops.** A device with no machine UUID
**cannot participate in coordination at all** — there is no identity to key a
claim on, so permitting the run is a deliberate, stated trade. A registered device
that merely cannot *reach* coord right now is a full participant whose peers are
simply invisible — the case where proceeding is most dangerous. Same observable
("no claim acquired"), **opposite** correct response. Do not let the shared
observable collapse the two branches into one: that is the
`silent-empty-is-unknown` class (served policy `verification-and-evidence`)
applied to a mutex — *"I could not ask whether anyone holds this"* is being read
as *"nobody holds this."*

#### Resource key (identity is already resolved)

`machine_id`, `$AGENT_SESSION_ID`, the plan-stem and the coord HTTP base were all
resolved ONCE at **Step 0.48** and are reused verbatim here — do not re-resolve
them, and do not re-derive them per phase. Step 0.48 is the single implementation
of that chain (it is `/preflight`'s, with the owner-token fields `/preflight`'s
own HTTP fallback omits); this step adds exactly one thing on top of it:

- **Resource key.** `plan:<plan-stem>:phase:<phase-number>` — e.g.
  `plan:2026-05-18-agent-spawn-coordination:phase:3`. That is the Step 0.48
  reserve key with `:phase:<n>` appended. The phase claim is **nested under** the
  plan reserve, not an alternative to it: the reserve says "this document is mine
  to move", this claim says "this phase's agent is mine to spawn".

The owner token `<machine_id>:<agent_session_id>` Step 0.48 resolved is what makes
a SECOND `/implement-plan` on the SAME machine see a structured `held` conflict
instead of silently taking over this session's phase claim — plan
`2026-06-03-coord-session-scoped-claim-owner-plan` (SHIPPED 2026-06-03; coord
PR #271 + qontinui-claude-config PR #49). Send it on every acquire, heartbeat and
release below, and omit the field entirely (never an empty string) if
`$AGENT_SESSION_ID` resolved empty.

#### Pre-flight call

For each phase, issue (via the Bash tool):

```bash
curl -fsS -X POST "$COORD_HTTP_URL/claims/acquire" \
  -H "Content-Type: application/json" \
  -d "$(cat <<EOF
{
  "kind": "phase",
  "resource_key": "plan:<plan-stem>:phase:<n>",
  "machine_id": "<machine_id>",
  "agent_session_id": "$AGENT_SESSION_ID",
  "metadata": {
    "plan": "<absolute-plan-path>",
    "phase": <n>,
    "skill": "implement-plan"
  }
}
EOF
)"
```

(Omit the `agent_session_id` line entirely if `$AGENT_SESSION_ID` resolved
empty — don't send an empty string.)

Per `coord/src/claims.rs` the response's `result` discriminator is
snake_case-tagged. Parse it:

- `"claimed"` / `"renewed"` → claim acquired. Capture the response's
  `correlation_id` if present (the spawned child agent will heartbeat
  against this claim via `/claims/heartbeat`). **Add the claim to the
  heartbeat ledger** — on `renewed` as well as `claimed`, because unlike
  the plan reserve a phase claim is only ever renewed by this same session,
  so there is no outer owner to defer to:

  ```bash
  bash <workspace-root>/qontinui-claude-config/scripts/coord-claim-heartbeat.sh add \
    --ledger "$CLAIM_LEDGER" \
    --kind phase \
    --key "plan:<plan-stem>:phase:<n>"
  ```

  `$CLAIM_LEDGER` is the path Step 0.48 resolved; the loop it started picks
  the new row up on its next tick, so no second `start` is needed (and if
  Step 0.48 returned `renewed` and started no loop, `add` is still correct —
  issue `start` once here so the phase rows are covered). **Then cancel any
  pending continuation for this plan (takeover — see below)**, and proceed
  to launch the phase agent for this phase.
- `"held"` → another agent already holds the claim. **DO NOT launch
  the phase agent.** Enter the conflict resolution flow below.
- `"topic_conflict"` / `"topic_unknown"` / `"invalid_topic"` → surface
  the error verbatim to the operator and abort the skill. These are
  not expected from the call shape above but handle defensively.
- **Anything that is not a parsed result at all** — connection error,
  timeout, non-2xx, or a body that does not parse — on a device that DID
  resolve a machine UUID:

  > This is **UNKNOWN, not free.** Do **not** launch the phase agent. Report
  > the transport failure verbatim, run `/coord-revive`, and re-issue over the
  > door it reports LIVE. If no door is live, surface to the operator via
  > `AskUserQuestion` (**Abort** / **Proceed uncoordinated**) — proceeding is a
  > decision someone makes, never a default reached by falling through an
  > undocumented branch.

  Before this arm existed, a `curl -fsS` that timed out, DNS-failed, 401'd,
  502'd or returned an unparseable body matched **none** of the documented
  outcomes, so the run simply continued — the same result as the deliberate
  `machine_id` skip above, reached by an undocumented path. That is failing
  OPEN. Step 0.48 states the identical arm for the plan reserve; it is one rule
  at two granularities.

#### Cancel and mute a pending continuation on takeover

Taking the phase claim directly means THIS session is doing the work a
**vet→implement safety-net** gate's continuation may have queued as a fresh
runner-terminal spawn. Leaving that continuation alive means the runner spawns a
**redundant terminal** on its next WS reconnect (its in-process dedupe set is
forgotten across restarts, and the pending row survives up to **7 days** — the
window was widened from 24h on 2026-07-23).

**Normally Step 0.5 already did this** — it cancels-and-mutes at the IN PROGRESS
stamp, which is earlier, and that earlier retirement is what makes `/vet-plan`
§5.4's `/vet-imp` safety net safe. This copy is the backstop for the paths
where Step 0.5 did not run or did not land: a non-coord environment, a
transport failure, or a gate that only became pending between the stamp and here.
If nothing is pending, say "no pending continuation to cancel" — do not report a
second cancellation of the same gate.

**Same two calls, same order, same doors as Step 0.5: cancel, then mute.** Since
2026-08-30 the continuation rides a separate `time_elapsed` net gate under
`phase_name` `"vet→implement safety net"`, not the `unit_ready` record gate, so
the row you act on is whichever one carries a `continuation_spawn` — and it is
normally still **pre-dispatch**, which the cancel handles (`cancel_continuation`
is deliberately unguarded on `continuation_dispatched_at`). The mute is what stops
the retired net blocking `unit_ready` as an open sibling.

So, **best-effort, right after the FIRST phase claim of this run succeeds** (do it
once per run, not per phase):

1. Resolve the `work_unit_id` for this plan-stem via
   `GET $COORD_HTTP_URL/coord/agent-work-units/<plan-stem>` (or the upsert used
   elsewhere). Take **`.work_unit.id`** — that response is
   `{work_unit, recent_history, citations}`, **not a bare unit**, so reading a
   top-level `id` yields nothing and the anchor silently comes out empty. Use the
   **`agent-`** path — the operator `GET /coord/work-units/<plan-stem>` is
   `TenantId`-only and answers this session's device JWT with
   **403 `tenant_not_resolved`**. The split is **by VERB, not by prefix**: the
   `POST` writes under `/coord/work-units/…` (`/upsert`, `/transition`,
   `/register-gate`, …) *are* device-authed — only the `GET`s moved to the
   `agent-` twins.
2. Query the work unit's gates for a live continuation:
   `GET $COORD_HTTP_URL/coord/agent-gates?work_unit_id=<id>` → rows carrying a
   `continuation_spawn` with `continuation_consumed_at == null ∧
   continuation_cancelled_at == null`. **Do not filter on
   `continuation_dispatched_at != null`** — that filter drops the pre-dispatch
   rows, which under the 30-minute net are the common case, and pre-dispatch is
   exactly the state the cancel was built to stamp.
3. For each such gate, fire the cancel. **Two agent-side transports, one
   capability:** `coord_cancel_continuation` `{gate_id, reason}` is the native
   MCP tool and is the shorter path when it is visible; the REST recipe below is
   the **`/agent/` infix twin** of it — same `cancel_continuation_inner`, so use
   whichever is alive, and a masked MCP tool is masking, not a closed path.
   Either way (device or agent JWT) tenant and `cancelled_by` derive server-side
   from the token, so the body carries `reason` alone — an implementing session
   holds a device JWT, so the unprefixed `TenantId` routes are the operator-side
   equivalents and are not yours to call:

   ```bash
   curl -fsS -X POST "$COORD_HTTP_URL/coord/gates/<gate_id>/agent/continuation-cancel" \
     -H "Content-Type: application/json" \
     -H @"$HDR_FILE" \
     -d "$(cat <<EOF
   { "reason": "taken over by session $AGENT_SESSION_ID" }
   EOF
   )"
   ```

   `$HDR_FILE` is a private tempfile holding the `Authorization: Bearer <jwt>`
   line, written with the `printf` **builtin** so the token never reaches a child
   process's argv — every peer session on the machine can read a cmdline. Reach
   the route over the `/coord-mcp` proxy instead and no header is needed at all:
   the runner injects a live device JWT.
4. **Then mute the same gate.** Same two-transport shape as the cancel:
   `coord_mute_gate` `{gate_id}` is the native MCP tool, and the REST twin is
   `POST "$COORD_HTTP_URL/coord/gates/<gate_id>/agent/mute"` with the same
   headers — a masked MCP tool is masking, not a closed path. The cancel
   writes only the `continuation_*` columns and leaves the **verdict untouched**,
   so without the mute the net gate stays `open` and, on a standalone-vetted unit
   that has one, pins the `unit_ready` record gate `Open` with a *"1 sibling
   gate(s) still open"* reason indefinitely. Mute the gate that carried the
   continuation, never `unit_ready` — and never **withdraw** a `unit_ready` gate
   either: a `withdrawn` row is not `cleared`, so it permanently blocks that
   unit from deriving `ready`.

This is **best-effort and MUST NOT block** the phase launch: a non-2xx, a 404
(no such gate), or a network failure is fine —
narrate it and proceed. A **409 `already_consumed`** means a spawn already
happened: report it honestly (do not claim a clean takeover), still mute the
gate, and still proceed with this session's work. Narrate the outcome either
way — "cancelled and muted net gate `<gate_id>`" or "no pending continuation to
cancel".

(canonical spec: `_gate-registration` → "Continuation cancel + refresh" — keep in sync)

#### Conflict resolution flow (on `"held"`)

Surface to the operator (text-mode UI — `/implement-plan` runs in the
terminal, no webview). The `held` response carries `current_holder` (the
holder's machine_id) and, when the holder is session-scoped,
`current_holder_session`. When `current_holder` equals THIS machine and
`current_holder_session` differs from `$AGENT_SESSION_ID`, the conflict is
**another session on THIS machine** — say so explicitly rather than
implying a different box:

```
Another session on THIS machine (session <current_holder_session>) is
already implementing plan:<plan-stem>:phase:<n>.
```

Otherwise (different machine, or a legacy holder with no session):

```
Another agent (machine <current_holder>) is already implementing
plan:<plan-stem>:phase:<n>.
```

Then, in both cases:

```
Options:
  (1) Abort — stop the implement-plan chain
  (2) Wait  — poll every 30s until the claim clears, then resume
              (default timeout 30 min; override with --wait-timeout=<Nm>)
  (3) Steal — revoke the other agent's claim (admin OR same-machine
              originator only)
```

Use `AskUserQuestion` with header `Claim conflict` and the three options.
Handle the selection:

- **Abort.** Stop the skill. Do not launch any further phase agents
  even for phases that DID acquire a claim — release those before
  exiting (see "Claim release" below).
- **Wait.** Poll the claims by-resource read every 30 seconds with
  `kind=phase&key=<rk>` (the query param is `key`, not `resource_key` —
  `routes.rs::ByResourceQuery`; URL-encode the `<rk>` value, which contains
  `:`). **Credential the read dual-shape** (claims-read-auth-hardening):
  read the workspace `.mcp.json` and branch on its `coord-mcp` entry —
  - *Proxy shape* (device-provisioned session: loopback
    `http://127.0.0.1:<port>/coord-mcp` url + a nonce under
    `X-Coord-Mcp-Proxy-Key` **or** `Authorization` — accept BOTH; the header
    moved in Phase 2 of plan
    `2026-08-20-coord-mcp-reconnect-dcr-and-restart-orphaning` and the legacy
    name stays accepted): `curl GET <proxy_base>/claims/by-resource?kind=phase&key=<rk>`
    with whichever of those two headers the config carries (the runner injects a
    live device JWT).

    **Static-bearer detection:** an `Authorization` header alone no longer
    proves the static-bearer shape — a proxy nonce now travels there too. The
    discriminator is whether the token is JWT-shaped.
  - *Static-bearer shape* (agent-spawn session: real coord url +
    `Authorization` header): `curl GET
    $COORD_HTTP_URL/coord/claims/by-resource?...` with that
    `Authorization: Bearer` header. Never route an agent bearer through
    the device proxy (scope elevation).
  - *Neither shape / no `.mcp.json`*: today's anonymous
    `curl GET $COORD_HTTP_URL/coord/claims/by-resource?...` (works until
    `COORD_CLAIMS_READ_AUTH_REQUIRED` enforcement arms). On a failed
    credentialed call, fail open to the anonymous form once.

  The endpoint returns `Option<ClaimHolder>` (now including the
  holder's `session_id`); when it returns `null` / no holder, retry
  `/claims/acquire` once. If acquire succeeds, proceed. Bound the wait by `--wait-timeout=<Nm>` from
  `$ARGUMENTS` (default 30 min). On timeout, ask the operator again
  (abort/wait/steal).
- **Steal.** Ask for a free-text reason (default: `"operator initiated steal"`),
  then call:

  ```bash
  curl -fsS -X POST "$COORD_HTTP_URL/coord/claims/steal" \
    -H "Content-Type: application/json" \
    -d "$(cat <<'EOF'
  {
    "kind": "phase",
    "resource_key": "plan:<plan-stem>:phase:<n>",
    "machine_id": "<machine_id>",
    "reason": "<reason>"
  }
  EOF
  )"
  ```

  On success, retry `/claims/acquire` — it should return `"claimed"`.
  On 403 (not admin AND not originator), surface the error and ask
  again (abort/wait — steal is not available). The coord side emits
  `events.coord.claim.stolen.machine.<displaced_machine_id>` so the
  displaced agent's runner will surface a stolen-claim banner.

#### Claim release on phase completion

After each phase agent reports — whether success OR failure — **first drop
the row from the heartbeat ledger**, so the loop stops renewing a phase key
this run is finished with:

```bash
bash <workspace-root>/qontinui-claude-config/scripts/coord-claim-heartbeat.sh remove \
  --ledger "$CLAIM_LEDGER" --kind phase --key "plan:<plan-stem>:phase:<n>"
```

Then release the claim:

```bash
curl -fsS -X POST "$COORD_HTTP_URL/claims/release" \
  -H "Content-Type: application/json" \
  -d "$(cat <<EOF
{
  "kind": "phase",
  "resource_key": "plan:<plan-stem>:phase:<n>",
  "machine_id": "<machine_id>",
  "agent_session_id": "$AGENT_SESSION_ID"
}
EOF
)"
```

Release MUST carry the SAME `agent_session_id` used at acquire — the
owner token is the match key, so a release that omits it (or sends a
different session) will not match a session-scoped claim and returns
`"not_held"` instead of `"released"`. (Omit the line if `$AGENT_SESSION_ID`
is empty, exactly as at acquire.) The same applies to any
`/claims/heartbeat` the spawned phase agent sends — it must reuse the
inherited `$AGENT_SESSION_ID`.

The release endpoint is idempotent, but **`"not_held"` is evidence the claim
LAPSED, not a clean no-op** — it used to be the expected answer for any phase
running past its TTL, because nothing heartbeated. With the ledger loop above
running, it no longer is: reaching it means the loop died, the claim was
stolen, or the row was never added. Report it, and read
`bash <workspace-root>/qontinui-claude-config/scripts/coord-claim-heartbeat.sh status --ledger "$CLAIM_LEDGER"` to say
since when. Treat release as
try/finally semantics: release MUST fire even on phase-agent failure,
even on `/implement-plan` skill abort. If the operator chose Abort in
the conflict-resolution flow, release every claim this session already
acquired for earlier phases before exiting.

Phase claims have a 7200s (2 hour) default TTL per `claims.rs:121`.

**This skill DOES auto-heartbeat between phase launches.** The ledger loop
started at Step 0.48 (`scripts/coord-claim-heartbeat.sh start`) covers every
row added to `$CLAIM_LEDGER` — the plan reserve and each phase claim alike —
re-heartbeating at min(row TTL)/3 with a 60 s floor, which for a 7200 s phase
claim is one request per 2400 s. The spawned phase agent does not need to
heartbeat its own claim, and a phase running longer than 2 hours is no longer
a special case. The loop replays the owner token on every request; a hand
heartbeat must too, or it will not match and returns `not_held`.

**Read `status` at every phase boundary.** The loop can die — a killed pid, a
`stolen` result that made it exit deliberately, a box that slept. So before
launching the NEXT phase, run:

```bash
bash <workspace-root>/qontinui-claude-config/scripts/coord-claim-heartbeat.sh status --ledger "$CLAIM_LEDGER"
```

`LIVE` (exit 0) is the only verdict that means the claims are held. `STALE`
(3), `DEAD` (4) and `STOLEN` (5) each mean the claim is **UNKNOWN, not held**
— re-`acquire` the affected key before launching, treat a foreign `held` on
the re-acquire as the conflict flow above, and **say in the report which
verdict you saw and what you re-acquired**. Silence here is the
`silent-empty-is-unknown` failure: a dead loop looks exactly like a healthy
one to anything that does not ask.

#### `/loop` gap (documented limitation)

`/loop` is built into Claude Code itself, NOT a user-editable slash
command in `~/.claude/skills/` or `.claude/commands/`. As of 2026-05-18
there is no way to inject this pre-flight into `/loop`-spawned phase
agents from the skill layer. Operators using `/loop` for plan-phase work
should either:

- Manually pre-flight via the same `curl` shape above before invoking
  `/loop`, and release on completion, OR
- Wait for a future Claude Code update that exposes `/loop` as an
  editable skill or adds a pre-flight hook mechanism, OR
- Use `/implement-plan` directly for plan-phase work (this skill is the
  canonical plan-driven entrypoint and IS pre-flight-coordinated).

The runner-side spawn flow (Phase 3 of the agent-spawn-coordination
plan) covers Tauri-IDE-initiated spawns through the same `/claims/acquire`
gate; `/loop` is the remaining un-coordinated entrypoint.

### Step 0.6.5: Publish activity to `coord.device_status`

After acquiring the phase claim, UPSERT a status row so the operator
dashboard's live "current activity" tile reflects what this agent is
doing right now. This is the read-side of Phase 1.1 + 1.3 of plan
`2026-05-21-coordination-improvements.md` — Phase 1.1 added the
`tenant_id` column on `coord.device_status`; Phase 1.3 wires the
dashboard's `MachineCard` to poll/subscribe `GET /coord/status?tenant_id=…`.
This step fills the rows the dashboard renders.

The UPSERT is keyed on `device_id`, so each new call overwrites the
prior row for this machine. That's the correct shape — only one task
can be "current" per machine at a time. The 1h `prune_stale()` job on
coord (`status.rs:171-184`) handles cleanup if a skill crashes without
clearing.

**Skip-and-warn for non-coord environments.** Mirrors Step 0.6 — if
`device_id` is not resolvable (env `QONTINUI_MACHINE_ID` unset AND
`~/.qontinui/machine.json` missing or unreadable), emit a single-line
warning and proceed. Status publication is observability, not gating.

**Failure handling.** Any non-2xx response to the POST is logged as a
single-line warning (`⚠️ coord status publish failed: <status> <body>`)
and the skill continues. NEVER abort the implement-plan chain on a
status publication error — the dashboard tile is observability, not
a gate.

#### Resolution chain (same identity sources as Step 0.48)

1. **`device_id`.** Already resolved at Step 0.48 — env `QONTINUI_MACHINE_ID`
   first, else `~/.qontinui/machine.json` parsed for `"device_id"` (the
   canonical name post-unified-devices) with `"machine_id"` as the legacy
   fallback. Reuse that UUID; do not re-resolve. The coord wire-field name is
   `device_id` on THIS route and `machine_id` on `/claims/*` — same UUID, two
   field names, neither following the local key it came from.
2. **`current_repo`.** The MAIN repo's directory name, resolved from the
   worktree the skill is executing in —
   `basename "$(dirname "$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null)")"`.
   NOT `basename` of `git rev-parse --show-toplevel`: from a linked git
   worktree that returns the WORKTREE's own directory name
   (`myrepo-wt-pr161-followup`, `myrepo-wt-lna`), so the
   dashboard tile groups this session under a repo that does not exist —
   and sessions run under `QONTINUI_AGENT_WORKTREE_MODE=1`, so that is the
   common path, not an edge case. `--git-common-dir` resolves to the main
   checkout's `.git` from a worktree and from the canonical checkout alike,
   and `--path-format=absolute` (git >= 2.31) keeps it from returning a
   relative `.git`. **If it prints nothing or `.`** — not a git tree — omit
   `current_repo` rather than sending what the expression then evaluates to
   (the parent directory's name, a wrong-but-plausible repo). (Inside a git
   submodule it yields `modules`; no submodules exist here.)
3. **`current_branch`.** `git symbolic-ref --short HEAD` from the same
   worktree.
4. **`tenant_id`.** Env `QONTINUI_TENANT_ID` if set; otherwise omit
   the field entirely (the coord column is nullable and will default
   to NULL).
5. **Coord HTTP base.** Env `COORD_HTTP_URL` first. Else
   `https://coord.qontinui.io` — same as Step 0.6.

#### Initial UPSERT (after the first phase claim acquires)

For the FIRST phase claim of this skill invocation, after the
`/claims/acquire` returned `"claimed"` or `"renewed"`, issue:

```bash
curl -fsS -X POST "$COORD_HTTP_URL/coord/status" \
  -H "Content-Type: application/json" \
  -d "$(cat <<EOF
{
  "device_id": "<device_id>",
  "current_task": "implement-plan: <plan-stem>",
  "current_repo": "<repo basename>",
  "current_branch": "<branch name>",
  "details": {"phase": "<n>/<total>"},
  "tenant_id": "<QONTINUI_TENANT_ID, omit field entirely if unset>"
}
EOF
)"
```

`<n>/<total>` reflects the FIRST phase number being launched and the
plan's total phase count from Step 0's checklist (e.g. `"1/14"`).

#### Phase-launch UPSERT (before each subsequent phase agent)

In Step 1, immediately BEFORE launching each phase agent (after that
phase's `/claims/acquire`), issue the same POST with `details.phase`
updated to that phase's `n/total`:

```bash
curl -fsS -X POST "$COORD_HTTP_URL/coord/status" \
  -H "Content-Type: application/json" \
  -d "$(cat <<EOF
{
  "device_id": "<device_id>",
  "current_task": "implement-plan: <plan-stem>",
  "current_repo": "<repo basename>",
  "current_branch": "<branch name>",
  "details": {"phase": "<n>/<total>"}
}
EOF
)"
```

For parallel phase launches, fire one UPSERT per phase sequentially
just before each Agent call — the rows overwrite each other quickly,
but the "most recent" task is what the dashboard tile shows, and
that's the right semantic for a single-machine fan-out.

#### Final UPSERT — clear on completion (Step 6)

When Step 6 (SHIPPED stamp, in place) completes successfully, POST one
final upsert that clears `current_task` so the dashboard tile stops
showing this plan as in-flight:

```bash
curl -fsS -X POST "$COORD_HTTP_URL/coord/status" \
  -H "Content-Type: application/json" \
  -d "$(cat <<EOF
{
  "device_id": "<device_id>",
  "current_task": null,
  "current_repo": "<repo basename>",
  "current_branch": "<branch name>",
  "details": {}
}
EOF
)"
```

If the skill aborts before Step 6 (e.g. operator chose Abort in the
Step 0.6 conflict-resolution flow, or a phase agent fails fatally),
fire this clearing POST best-effort alongside the `/claims/release`
calls in the same try/finally. If even that fails, the `prune_stale()`
TTL will clean it up within an hour.

#### `/loop` activity-publication gap

The same `/loop` limitation from Step 0.6 applies here: `/loop` has no
hook surface, so a `/loop`-spawned chain can't auto-publish its
activity. Operators driving plan-phase work via `/loop` should either
manually POST the same `/coord/status` shape above before invoking
`/loop` and clear it on completion, OR use `/implement-plan` directly
(this skill is status-instrumented end-to-end).

### Step 0.7: UI Bridge wire-through pre-flight

Before launching phase agents, scan the plan and the touched-files list for any of the SDK files below; if matched, include the reminder block in the agent prompt so the agent knows the SDK change has parallel runner layers it must wire through.

> **UI Bridge wire-through reminder.** If your changes touch any of:
> - `ui-bridge/packages/ui-bridge/src/server/handlers.ts`
> - `ui-bridge/packages/ui-bridge/src/server/types.ts` (especially `UI_BRIDGE_ROUTES`)
> - `ui-bridge/packages/ui-bridge/src/server/relay-handlers.ts`
> - `ui-bridge/packages/ui-bridge/src/react/commandHandlers.ts`
>
> …the change MUST also be wired through the runner's three parallel layers, or it will silently 404 / drop fields against a live runner:
> 1. **Direct HTTP handlers** in `qontinui-runner/src-tauri/src/mcp/ui_bridge/<family>.rs`. Register in the family's `routes()` AND `route_entries()` (per-family static manifest concatenated by `mod.rs::route_manifest()`).
> 2. **WS-transport outer wrappers** in `qontinui-runner/src-tauri/src/mcp/sdk_client.rs` for browser-based consumers under `/ui-bridge/sdk/*`.
> 3. **Frontend IPC bridge** in `qontinui-runner/src/hooks/ui-bridge-events/utils.ts` (and the family-specific `use*Events.ts` siblings) when the route depends on data only the React frontend has.
>
> Verify with: `cargo test manifest_matches_route_calls` (internal drift) AND `cargo test sdk_manifest_routes_are_exposed_by_runner` (SDK↔runner diff — Phase 2a). See `qontinui-runner/src-tauri/src/mcp/ui_bridge/CONTRACT.md` for the full per-route classification.
>
> For deeper assurance — query-param drops, field-stripping, status-code mismatches that the manifest diff can't see — run `pwsh qontinui-runner/scripts/contract-smoke.ps1` against a live supervisor (port 9875). It spawns a temp runner, hits every `UI_BRIDGE_ROUTES` entry, asserts the three known shape contracts (`revealsAny=` filters, `scope` round-trips, `expect` returns 422 on timeout), and stops the runner cleanly. ~3-5 minutes; required if your changes alter response shape, query handling, or status-code mapping.

### Step 0.7.5: Semantic-resource reserve handshake (predict-then-reserve)

Before a phase agent **authors** a change to a known **shared semantic
resource** that cannot be mechanically re-pointed at land, it must reserve
that resource through coord first — so two concurrent sessions don't
hand-pick the same registry slot and fork `main`. This is the read-side of
the auto-reconcile + handshake layer (plan
`2026-06-02-coord-conflict-autoreconcile-and-agent-handshake`); it pairs
with the auto-rebase that re-points a loser when a fork *does* slip through.

**Migrations are NO LONGER mandatory-reserve** (plan
`2026-06-25-migration-ordering-land-time-repoint`). An alembic migration's
`down_revision` is a mechanical, conflict-free link that coord re-points to
the live merged head **at land time** (the land-time re-point engine), and
the `alembic-graph-pr.yml` CI check fails any PR that would fork the chain.
So a phase that authors a migration just chains off its **local head** and
pushes — no reserve-before-author handshake, no `down_revision` assignment,
no bind. `coord_migration_reserve` still exists as an **optional advisory**
call (returns a suggested `down_revision` + a "you're stacked behind #N"
heads-up); use it for the early signal if convenient, but it gates nothing.

**Mandatory-reserve scope.** A resource is mandatory-reserve iff coord has
a **registered `SemanticResource` grammar** for it AND it is NOT land-time
re-pointable. **Ask the registry — do not read a list here:**

```bash
curl -s "$COORD_HTTP_URL/coord/claims/semantic-resource-grammars"
```

Each entry carries `{class, key_shape, description, land_time_repointable}`,
and `mandatory_reserve` is that predicate evaluated — so a class becomes
mandatory the moment its grammar ships, with no edit to this file. The
reserve response echoes the same verdict under `grammar`.

> **This paragraph used to carry the list itself** — "Today that is exactly:
> the MCP tool registry", with enums and lockfiles called
> *tracked-but-not-yet-grammared* and therefore advisory — while also
> forbidding exactly such a hand-edited list. It could not do otherwise:
> **no `SemanticResource` grammar registry existed until 2026-08-25**
> (`git grep grammar -- crates/coord/src` found only `hot_file_grammars`,
> the unrelated merge-conflict file grammars). The classes lived in prose in
> a doc comment, in the tool description, and in a reject-list predicate. So
> "the registry is the single source of truth" named something unbuilt, and
> every mandatory-vs-advisory claim resting on it was UNKNOWN rather than
> false. The registry is now real and is the answer.

Registered today: `mcp-tool-registry` (`mcp-tool-registry:<mount>` — a
colliding tool slot cannot be re-pointed at merge, so pre-authoring
reservation is the only fork-prevention available), `enum`, `lockfile`,
`plan` (`plan:<plan-stem>` — see Step 0.48), and `migration-head`, which is
registered *and* land-time re-pointable, hence **not** mandatory and
additionally refused by reserve. Treat that sentence as a convenience gloss
with a shelf life; the route above is authoritative.

**Scan + inject.** Before launching each phase agent (alongside the
Step 0.7 UI-Bridge scan), check whether the phase's touched-files /
description authors a mandatory-reserve resource (the MCP tool registry /
enum / lockfile classes — NOT migrations). If so, include this block in the
agent prompt:

> **Reserve-before-author handshake.** Before you author this change,
> reserve the resource over MCP — the call differs by resource class:
>
> **Migration (alembic): NO reserve needed.** Just author your migration
> with `down_revision` = your **local** alembic head and push. coord
> re-points the `down_revision` to the live merged head at LAND time (the
> land-time re-point engine), and `alembic-graph-pr.yml` CI fails any PR
> that would fork the chain — those are the fork-prevention authority, not a
> reservation. OPTIONAL early signal only: `coord_migration_reserve(repo, revision)`
> (HTTP: `POST $COORD_HTTP_URL/coord/migrations/reserve`) returns an advisory
> suggested `down_revision` + queue position ("stacked behind #N") — handy to
> know if a sibling is in flight, but it binds nothing, expires nothing, and
> you never need to act on it. (The old `kind=alembic_revision` claim returns
> 410; the bind/withdraw flow is gone.)
> **Do NOT add a `coord:stacked-on` / `coord:upstream-of` label to order a
> migration stack.** coord derives the serialization edge
> (`EdgeKind::StackedOn`, `dep_graph.rs` `predict_migration_stacks`) from the
> `down_revision` chain automatically — the `down_revision` link IS the
> ordering. A hand-added label is redundant noise (and was a historical
> stale-block source); reserve `coord:stacked-on` / `coord:upstream-of` for
> genuine *code* stacks — one PR's source depends on another's, with no shared
> migration.
>
> **Tool registry / enum / lockfile:** call
> **`coord_reserve_resource(kind, name)`** — `kind` is the lowercase-kebab
> class (`mcp-tool-registry`, `enum`, `lockfile`); `name` is the instance
> (e.g. the mount `phase11`). coord keys a `semantic_resource` claim on
> `<kind>:<name>`. Give any `curl` fallback for this reserve
> **`--max-time 120`**, for the same reason Step 0.48 states: a budget under the
> reserve's cold cost turns a healthy coord into a fail-closed abort, and the
> MCP arm's own budget is not settable from a skill file.
>
> **Branch on the `coord_reserve_resource` result (tool-registry / enum /
> lockfile):**
> - **`Granted`** / **`claimed`** → you hold the reservation; proceed to author.
>   (Reserve answers **exclusion only** now — there is no `forking_siblings`
>   field to check here either; see the next bullet.)
> - **`Held { holder }`** → another agent owns it. **Do NOT hand-pick a
>   value.** Wait for release (poll `coord_claim_check`) or coordinate with
>   the holder, then re-reserve.
> - **`ForkRisk { siblings }` / `forking_siblings` — no longer returned.** The
>   same collision scan was moved off the synchronous reserve path for every
>   `semantic_resource` class, not just `plan` (plan
>   `2026-08-26-mandatory-plan-reserve-cold-cost-trips-its-own-fail-closed-arm`
>   Phase 2). Do not branch on an outcome that can no longer fire. If you want
>   to know whether sibling PRs are racing this resource, ask
>   **`coord_predict_resource_collisions`** explicitly BEFORE picking a value —
>   and when it names siblings, do **not** pick a value off the old head: chain
>   off the current head or coordinate with them, so you don't author the fork.
>
> For the tool-registry path, heartbeat (`coord_claim_heartbeat`) if
> authoring takes a while; release (`coord_claim_release`) once your commit
> that claims the slot lands. (Migrations need none of this — there is no
> reservation lifecycle; land-time re-point handles chain order.)

The semantic-resource tools live on coord's `phase11` MCP mount
(`coord_claim_acquire/heartbeat/release/check`, `coord_reserve_resource`);
the advisory migration tools are `coord_migration_reserve` (optional early
signal) and `coord_migration_queue` (read the advisory queue) — the
`_bind_pr` / `_withdraw` tools were retired with the bind gate. If a phase
agent has no MCP access to coord, fall back to the HTTP API: the optional
migration advisory is `POST $COORD_HTTP_URL/coord/migrations/reserve`
(returns a suggested `down_revision`); other semantic resources use
`POST $COORD_HTTP_URL/claims/acquire` with
`kind: "semantic_resource", resource_key: "<class>:<name>"`.

**Skip-and-warn for non-coord environments.** Mirrors Step 0.6 — if no
`machine_id` / coord base is resolvable, emit a single-line warning
(`⚠️ reserve handshake skipped: no coord available`) and let the phase
proceed without a reservation. The reserve is coordination, not a hard
gate (the CI gate + auto-rebase are the backstops); never block authoring
on a reserve failure.

### Step 0.7.6: Edit-effect loop wire-through (predict → gate → verify)

Include this block in EVERY phase agent's prompt — it wires the phase
agent into coord's edit-effect D3 loop (plan
`2026-06-05-edit-effect-loop-adoption`). The gate is advisory: it never
blocks a phase, it informs the coordinator's decision.

> **Edit-effect loop — predict, gate, verify.** Run coord's edit-effect
> loop around your edits. Every call is **best-effort**: a failed or
> unreachable coord NEVER blocks the phase — warn once and proceed.
> `COORD_HTTP_URL` overrides the base (default `https://coord.qontinui.io`).
>
> **1. Pre-edit (after worktree allocation, before the first `Edit`):**
> call predict-and-check with `{repo: "<repo basename>", paths: <the
> phase's planned touched files>, head_sha: "<git rev-parse HEAD in the
> worktree>", declared_globs: <plan/work-plan globs when present>,
> declared_intent: "<plan-stem>: <phase title>"}`.
> - **MCP (preferred):** `coord_edit_predict_and_check(<same JSON>)`.
> - **HTTP fallback (universal):** `curl -fsS -X POST
>   "$COORD_HTTP_URL/coord/edits/predict-and-check" -H "Content-Type:
>   application/json" -d '<same JSON>'`.
>
> **2. Branch on the response envelope** (`{predicted_effect, resolution,
> risk_factors}`). Read `resolution.action`: anything that is **not**
> explicitly `escalate` (e.g. `proceed`) → continue silently. On
> `escalate` → list the `risk_factors` strings; proceed only when EVERY
> factor is blast-radius-shaped AND the plan's file inventory explicitly
> scopes that many files. Otherwise STOP and report the factors verbatim
> to the coordinator in your phase report — the coordinator applies the
> decision framework (the gate creates no `agent_questions` row; you
> never ask the operator directly).
>
> **3. Post-edit verify (after the phase commit):** call verify with
> `{repo, paths: <files actually touched>, head_sha: "<the new commit
> sha>", tests_predicted: <the predict response's `detail.affected_tests`,
> when present>}` — MCP `coord_edit_verify(<JSON>)` or HTTP `curl -fsS -X
> POST "$COORD_HTTP_URL/coord/edits/verify" …`. Record the composed
> outcome (`composed_outcome` + the per-subspace summary) in your phase
> report. A `Contradiction`/`Failure` composed outcome is a phase-report
> **red flag**, NOT an automatic revert.

### Step 0.7.7: Subagent stall contract

Include this block in EVERY phase agent's prompt — it is the prompt-side
half of the subagent stall watchdog (plan
`2026-07-03-subagent-stall-watchdog`). The observed failure class it kills:
an agent backgrounds a long build, ends its turn "awaiting notification,"
the wake-up never fires, and the finished work sits uncollected.

> **Stall contract — never wait passively, always resume idempotently,
> check in when you can.**
>
> **1. Never end a turn purely "awaiting notification."** Any
> backgrounded or long-running step (builds, test runs, CI waits) gets a
> work-type-appropriate fallback: re-check completion evidence (build
> artifacts, process state, output files) on a bounded timer and proceed
> on evidence. Completion notifications are an optimization, never a
> guarantee — the wake-up channel is at-most-once.
>
> **2. Idempotent resume.** On any nudge / system-reminder wake, FIRST
> re-check whether the awaited work already finished (evidence over
> memory), collect the result, and continue — never restart completed
> work.
>
> **3. Final report, not a progress note.** Your LAST message must begin
> with the sentinel line `FINAL-REPORT label=<your label> status=<STATUS>` —
> first line, plain text, nothing before it. Anything else is read as a
> progress note and you are resumed from your checkpoint. If you end a turn
> waiting on an external event, hold a live watcher and name it in your
> checkpoint's `blocked_on.watcher`, or say there that you hold none.
>
> **4. Check in.** When a `coord_expectation_checkin` MCP tool is
> available in the session AND your prompt carries an `expectation_id`,
> call `coord_expectation_checkin(expectation_id, progress_seq=<bumped>)`
> at each phase boundary. `progress_seq` is any monotonically increasing
> number of your choosing (phases completed, files edited — anything that
> only goes up); a higher value resets the supervision ladder and pushes
> your deadline out. Any `status_ask` message you receive MUST be answered
> via the same call (bump `progress_seq`, optionally add a one-line
> `note`). If the tool or the `expectation_id` is absent, skip silently
> (fail-soft) — never block phase work on it.
>
> **4. Checkpoint.** You hold a worktree and will commit on a branch, so you
> are in the population that must checkpoint. After each MATERIAL step —
> the claim, each commit, each push, each PR opened, each gate registered —
> rewrite your checkpoint (label = `<plan-stem>--phase-<n>`; the label is a
> FILENAME on every platform, so not the claim key's `:` form):
>
> ```bash
> cat <<'JSON' | bash <workspace-root>/qontinui-claude-config/scripts/agent-checkpoint.sh write --label <label>
> {"assignment": "...", "position": "what I just finished / what I am about to do",
>  "artifacts": {"worktrees": [], "branches": [], "prs": [], "plans": []},
>  "verified": [], "next_action": "the single next step, executable by a stranger",
>  "blocked_on": null}
> JSON
> ```
>
> It is the only thing that survives a provider-limit kill (a `429`, "You've
> reached your Fable limit"): your transcript is gone, and the coordinator is
> left with repo archaeology, which records what you DID and never what you
> were ABOUT TO DO. `knowledge-base/qontinui-specific/agent-checkpoints.md`.

**The coordinator's half of rule 4.** On EVERY phase-agent notification —
`failed` AND `completed` — read that agent's checkpoint BEFORE deciding
anything (`agent-checkpoint.sh read --label <label>`). A provider-limit
`failed` is not a verdict on the phase: re-spawn with the checkpoint passed
back verbatim. If the message states a reset time, do not re-spawn into the
limit: register a coord `time_elapsed` gate for it via `/gate` (convert the
stated time against the zone the message names, never the box's), then
`annotate` the checkpoint with `--key resume_gate_id` so a second kill does
not double-book — and read `annotations.resume_gate_id` back, verifying the
gate is still OPEN, before booking another. The reset-time PARSER that
automates that conversion is the plan's Phase 2 and is not landed at the time
of writing; until it is, the conversion is yours to do by hand. Exit `4` from
the read is UNKNOWN, not "nothing was done" —
fall back to inspection and say so. `status: completed` whose result is a
progress note rather than the structured summary is evidence the PROCESS
ended, not that the phase finished; treat it as unfinished.

### Step 0.8: Coordinator mode (when the plan scope is too large)

If you complained earlier that the plan scope is too large to implement directly,
**do not stop or hand back to the operator**. The main session pivots into
coordinator mode and ships the plan by orchestrating subagents. The coordinator
never writes feature code itself — its job is to spawn, review, decide, and
unblock.

#### Coordinator responsibilities

1. **Spawn.** For each phase (and, within a phase, each independently-buildable
   chunk if the phase itself is too large for one agent), launch an Agent with
   a self-contained prompt — full phase description from the plan, file paths,
   relevant context, and explicit instructions to implement fully (no stubs /
   TODOs), run type checks + lints, fix what it finds, and report a structured
   summary (files changed, decisions made, issues hit + how resolved, any
   remaining concerns). Launch independent phases / chunks in parallel via
   multiple Agent tool calls in a single message. Every spawned agent
   prompt MUST also contain the Step 0.7.7 stall-contract block (no
   passive waits, idempotent resume, fail-soft check-in) — the coordinator
   is the enforcement point for prompts it authors. At spawn time, if a
   `coord_expectation_register` MCP tool is available, call it once per
   spawned agent to register what you delegated:
   `coord_expectation_register(expected_kind='checkin_only',
   phase='<phase name>', note='<one line: what was delegated>')` — or
   `expected_kind='commit_on_branch'` with `expected_ref={repo, branch}`
   when the phase is expected to produce commits on a known branch. Keep
   the returned `expectation_id` and pass it to the phase agent in its
   prompt (its Step 0.7.7 check-ins reference it); when you collect that
   agent's result, call `coord_expectation_close(expectation_id)`
   (`status='met'` by default, `'cancelled'` if the phase was abandoned).
   If the tool is absent, skip silently — never block a spawn on it.
2. **Review.** When each agent returns, read its summary critically. Spot-check
   the actual diff with `git diff` / `Read` — don't trust the summary alone
   (see [[feedback_verify_function_exists_before_trusting_stamp]]). Confirm:
   the phase contract is met, no stubs were left behind, types/lints pass, no
   half-finished abstractions, no backward-compat shims, no dead code, no
   feature flags hiding incomplete work.
3. **Decide autonomously.** When an agent surfaces an ambiguity, conflict, or
   judgment call ("two ways to wire this — should I do A or B?"), DO NOT bounce
   it back to the operator. Resolve it using the decision framework below and
   issue the agent (or a follow-up agent) a concrete instruction. The operator
   asked for a coordinated implementation, not a stream of questions.
4. **Fix.** If an agent's output is wrong, incomplete, or violates the
   framework, fix it — either by editing directly in the main context (for
   small mechanical issues) or by spawning a follow-up agent with explicit
   instructions on what to change and why. Never accept "good enough" output
   that the framework would reject.
5. **Integrate.** After each wave of parallel agents, do a cross-phase
   integration pass in the main context: verify imports/exports line up,
   shared types are consistent across boundaries, no two agents introduced
   conflicting abstractions for the same concept. Reconcile divergences via
   direct edit or a targeted follow-up agent.

#### Decision framework

When weighing options as the coordinator — whether resolving an agent's
ambiguity, choosing between two implementation paths, or deciding whether to
accept an agent's output — optimize against these priorities, in order:

1. **Powerful features.** Prefer the option that unlocks more capability or
   composes better with planned future work. A more powerful primitive beats
   a narrower one even if the narrower one is "enough for now."
2. **Scalability.** Prefer the option that holds up as data volume, user
   count, concurrency, or call-site count grows. Reject choices that look
   fine at current scale but have a known cliff.
3. **Robustness.** Prefer the option that fails predictably, surfaces errors
   clearly, and recovers cleanly. Bias toward explicit invariants, structured
   errors, and verification at boundaries.
4. **Clean code.** Prefer the option that future readers will understand
   without archeology — clear names, focused functions, minimal indirection,
   no dead branches, no comments explaining what the code already says.

**Explicitly NOT factors:** programming effort (your time / token budget /
agent count is not a constraint here — ship the right thing), backward
compatibility (per CLAUDE.md, breaking changes are expected; delete-over-
deprecate; refactor fearlessly).

When two options tie on priorities 1–4, pick the one that leaves less
follow-up work for the next plan. If you genuinely cannot decide after
applying the framework, pick the option you'd defend in a code review and
note the trade-off in the phase commit message — do not stall the chain
asking the operator. (If priority-unresolvable decisions recur, propose
expanding the priority sets at wrap-up.)

#### Implementation priorities (execution)

The engineering priorities above — with the UX gates for user-facing
surfaces (memory: `ux-priorities-alongside-engineering`) — decide **what**
to build. A third orthogonal set, the **implementation priorities** (memory:
`implementation-priorities`), decides **how and when** the coordinator
executes, in order:

1. **Verified throughput.** Ship the most work that is built AND verified
   this session; unverified volume counts as zero. Verification is tiered
   by consumer: user-facing → goal observed on the page; consumer-free
   infra → green CI + the documented autonomous checks. Delegate the
   majority of the work to subagents to conserve coordinator context.
2. **Early risk retirement.** Sequence waves most-falsifiable-first — run
   the probe that can kill an assumption before the builds that depend on
   it.
3. **Autonomy with checks.** Proceed on checks, not permission. Merge,
   production deploy, migration, new security surfaces, cross-repo scope
   growth, and spend are all autonomous when their documented checks pass
   (no-live-users era; merge = no-reap + serialize, deploy = no-users,
   migrate = single head).
4. **Momentum through re-planning.** A falsified plan assumption never
   halts the session — see the escalation rules below.

#### When to escalate to the operator anyway

The coordinator is autonomous but not unconditional. Per the implementation
priorities, exactly two things justify an `AskUserQuestion`:

- **Operator-resource needs** — something only the operator can physically
  do: start the primary runner, unlock a phone, complete an interactive
  login, add a payment method.
- **Oversize-plan handoff** — a re-planned or combined plan too large even
  for coordinator-style orchestration: author it, vet it with a subagent,
  then present it for a fresh session.

Everything that used to be escalation-worthy is resolved in-session:

- **Falsified premise or goal-changing finding** (e.g., "the feature already
  exists under a different name") — re-evaluate against the priority sets
  and select the new correct path automatically. If it fits the original
  plan, incorporate it and keep building; if bigger, author a new/combined
  plan, vet it with a subagent, and execute it coordinator-style; only the
  oversize case above goes to the operator.
- **Production deploys / migrations / new security surfaces** — autonomous
  when the documented checks pass (see implementation priority 3).
- **Questions no priority set breaks** — decide yourself; by definition it
  is not important enough for the operator to have an opinion on. If this
  recurs, propose expanding the priority sets at wrap-up.

Destructive git and the rest of CLAUDE.md's "executing actions with care"
list still get care — prefer the reversible path — but care means checks,
not questions.

Routine implementation choices — library selection, API shape, file layout,
error-handling strategy, test structure — are NOT escalation triggers. Decide
and move on.

### Step 1: Implement All Phases (using subagents)

For each phase in the approved plan, **launch an Agent** (not a Skill call). The agent prompt must include:

1. The full phase description from the plan
2. The relevant file paths and context the agent needs
3. Instructions to: implement fully (no stubs/TODOs), run type checks/lints after, fix any issues found, and report what was changed

**Coord claim pre-flight (per Step 0.6).** Immediately BEFORE launching
the Agent for a given phase, run the Step 0.6 pre-flight for THAT phase
(`POST /claims/acquire` with `kind=phase, resource_key=plan:<stem>:phase:<n>`),
and `add` the acquired key to the heartbeat ledger per Step 0.6.
If `"held"`, resolve the conflict (abort/wait/steal) before proceeding to
the Agent launch. When launching phases in parallel, pre-flight each
phase's claim sequentially first (parallel acquires against distinct
resource keys are safe but easier to surface conflicts on linearly),
then launch the surviving phase Agents in parallel.

**Heartbeat status read at the phase boundary (per Step 0.6).** Before that
pre-flight — i.e. at every phase boundary, not once per run — read the ledger:

```bash
bash <workspace-root>/qontinui-claude-config/scripts/coord-claim-heartbeat.sh status --ledger "$CLAIM_LEDGER"
```

`LIVE` (exit 0) is the only verdict under which the claims this run already
holds — the Step 0.48 plan reserve included — are actually held. `STALE` (3),
`DEAD` (4) and `STOLEN` (5) each mean those claims are **UNKNOWN**, so
**re-`acquire` every key the ledger lists before launching this phase**
(`kind=semantic_resource, plan:<stem>` and each live `kind=phase` row), handle a
foreign `held` through the Step 0.6 conflict flow, and restart the loop with
`start`. **Say so in the report**: which verdict was read, which keys were
re-acquired, and what the re-acquire answered. A re-acquire that is not reported
is indistinguishable from a claim that never lapsed.

**Agent token refresh (rule (b), per Step 0.48).** Also before each phase
launch, run `bash <workspace-root>/qontinui-claude-config/scripts/coord-agent-refresh.sh` and act on its one verdict
line. A `CREDENTIAL_ONLY` / `EXPIRED` / `REFUSED` verdict here means the phase
agent will inherit a bearer that cannot write to coord — surface it before
launching, never after.

**Coord activity UPSERT (per Step 0.6.5).** Immediately AFTER each
phase's `/claims/acquire` succeeds and BEFORE launching that phase's
Agent, fire the Step 0.6.5 phase-launch UPSERT with `details.phase`
set to that phase's `<n>/<total>`. Failure is non-fatal (warn-and-
continue per Step 0.6.5 rules). For parallel launches: fire the
UPSERTs sequentially in launch order, then launch the Agents in
parallel.

**Reserve handshake (per Step 0.7.5).** If a phase authors a
mandatory-reserve semantic resource (a tool-registry / enum / lockfile
change — NOT a migration), include the Step 0.7.5 reserve-before-author
block in that phase's Agent prompt so the agent reserves the resource
before authoring — never hand-picking a colliding value. Migrations need
no reserve: they author against the local head and coord re-points at land.

**Edit-effect loop (per Step 0.7.6).** Include the Step 0.7.6
predict→gate→verify block in EVERY phase Agent prompt so the agent
predicts before its first edit, surfaces any `escalate` risk_factors to
you, and verifies after its commit. Best-effort — never blocks a launch.

**Stall contract (per Step 0.7.7).** Include the Step 0.7.7 stall-contract
block in EVERY phase Agent prompt so the agent never ends a turn purely
"awaiting notification" on backgrounded work, resumes idempotently
(evidence over memory) on any nudge/wake, and checks in via
`coord_expectation_checkin(expectation_id, progress_seq=<bumped>)` at each
phase boundary when that tool is available (fail-soft when absent).

**Checkpoint (per Step 0.7.7 rule 4).** The same block carries the
checkpoint contract, so every phase agent writes
`~/.qontinui/agent-checkpoints/<this session>/<label>.json` after each
material step. When you collect an agent — `failed` OR `completed` — read
that checkpoint first (`agent-checkpoint.sh read --label <label>`); a
provider-limit kill is re-spawned from it, never reconstructed from branches
and worktrees, and a stated reset time becomes a coord `time_elapsed` gate
recorded on the checkpoint with `annotate --key resume_gate_id`
(`knowledge-base/qontinui-specific/agent-checkpoints.md`).

**Expectation register (per Step 0.7.7).** At spawn time for each phase
Agent, if a `coord_expectation_register` MCP tool is available, call it
once per spawned agent:
`coord_expectation_register(expected_kind='checkin_only',
phase='<phase name>', note='<one line: what was delegated>')` — or
`expected_kind='commit_on_branch'` with `expected_ref={repo, branch}` when
the phase is expected to produce commits on a known branch. Keep the
returned `expectation_id` and pass it into that agent's prompt so its
Step 0.7.7 check-ins reference it. When you collect the agent's result,
call `coord_expectation_close(expectation_id)` (`status='met'` by default,
`'cancelled'` if the phase was abandoned). If the tool is absent, skip
silently — never block a spawn on it.

**Launch independent phases in parallel** using multiple Agent tool calls in a single message. Only serialize phases that have true dependencies on each other.

Each agent should:
- Implement the phase completely
- Run type checks and lints (`cargo check --all-targets`, `npx tsc --noEmit`, `ruff check`, etc.)
- Fix any errors or warnings
- Report back: files changed, what was implemented, any issues found and fixed
- **Begin the final message with the sentinel line** `FINAL-REPORT
  label=<phase label> status=<STATUS>` — first line, plain text, nothing
  before it (`bash <workspace-root>/qontinui-claude-config/scripts/agent-report-verdict.sh template`
  prints the block to paste into the prompt). A terminal message that does
  not begin with it is UNFINISHED, never a verdict.

**Classify every phase agent's result before acting on it (Phase 3 of plan
`2026-09-03-provider-limit-kills-destroy-subagent-context-and-nothing-resumes`).**
`status: completed` is evidence the PROCESS ended, not that the phase
finished. On EVERY notification — `failed` and `completed` alike — run
`bash <workspace-root>/qontinui-claude-config/scripts/agent-report-verdict.sh classify --expect-label <phase label> --checkpoint <<'MSG'`
with the agent's final message verbatim on the heredoc's lines (a
single-quoted `--message` cannot carry the apostrophes every real message
has).
`FINAL_REPORT` (exit 0): review the diff and continue. `PROGRESS_NOTE` (exit
3) or `MISLABELLED` (exit 6): the phase is UNFINISHED — re-spawn it with the
checkpoint at `RESUME_FROM` passed back verbatim rather than reconstructing
its state. `UNKNOWN` (exit 4): nothing to act on; say so. A `failed`
notification carrying a provider `429` with a stated reset is not retried
blind: run `scripts/rate-limit-reset.sh schedule` **with `--hint`** (see
`/vet-imp-sweep` Step 6 — without a brief the resumed agent's whole context is
`run /<skill> <args>`) so the resume is a coord `time_elapsed` gate that
survives this session, and quote its `gate_id`; a `529`/"Overloaded" is
`TRANSIENT` — retry once.

**Claim release (per Step 0.6).** AFTER each phase Agent returns —
success OR failure — `remove` that phase's row from the heartbeat ledger
(`bash <workspace-root>/qontinui-claude-config/scripts/coord-claim-heartbeat.sh remove --ledger "$CLAIM_LEDGER"
--kind phase --key "plan:<plan-stem>:phase:<n>"`) and THEN release that
phase's claim via `POST /claims/release`. The `remove` comes first so the
loop cannot renew a key between the release and the next tick. Treat both
as try/finally: they MUST fire even on agent failure or exception. On skill
abort, remove and release every claim this session acquired for any phase
before exiting.

After all phase agents complete, do a quick integration check in the main context:
- Verify cross-phase wiring (imports, exports, type consistency across boundaries)
- Run a combined type check/lint across affected repos
- Fix any integration issues directly

### Step 2: Manual Testing (if UI changes exist)

If the feature has UI or runner-facing changes, **invoke `/manual-test` using the Skill tool:**

```
Skill: manual-test
Args: <describe what to test based on the implemented features>
```

Fix any errors found. Re-invoke `/manual-test` after fixes. Repeat until passing.

Skip this step if the feature is purely backend/library code with no UI changes.

> **Runner UI changes — verify on a temp runner; NEVER the primary, NEVER stall, NEVER ask the operator.** This step is MANDATORY for a runner-facing UI change before the plan can be called done — a UX change is only verified by observing it rendered ([[feedback_verify_goal_on_page_not_inference]]). Do NOT leave the PR "pending on-device verification," do NOT propose that building+running the change "would disrupt the primary," and do NOT ask the operator to verify — all three are false and block autonomous development. The supervisor on `:9875` is INDEPENDENT of the primary on `:9876`: `POST /runners/spawn-test` builds the code into its own pool and spawns an isolated temp runner (port 9877+, own UI Bridge) with ZERO primary impact. Use `{"rebuild":true}` for origin/main, or slot-patch the worktree binary for PR/uncommitted code (`{"rebuild":false}`); drive the UI-Bridge visual check against the temp runner's port; `POST /runners/<id>/stop` when done. Full mechanics: `/manual-test` skill + memory `feedback_any_verification_uses_temp_runner_never_stall`.

### Step 3: Write Specs (if UI pages affected)

If any UI pages were created or modified, **invoke `/update-spec` using the Skill tool** for each affected page.

### Step 4: Commit

Use `/clean-commit` or commit manually. Commit trailers follow the harness
attribution rule: end the message with the `Co-Authored-By: <model>` and
`Claude-Session:` lines the harness supplies and add no other attribution
(aligned 2026-09-09; the previous "do NOT include AI attribution" line
contradicted the harness rule and main's own history).

**Cooperative abort-report (commit-action effect signatures §6.2).** If a
`git commit` is REJECTED by a pre-commit hook (non-zero exit), forward the
reason to coord before fixing + retrying:
`bash <workspace-root>/.claude/scripts/report-commit-abort.sh "<captured hook output>"`
— best-effort, fail-open; it never edits git or blocks. `/clean-commit` Phase 4
does this automatically; do the same on a manual commit. On machines with the
commit-abort wrapper installed
(operator-local installer `qontinui-dev-notes/scripts/install-commit-abort-hook.sh`, plan
`2026-06-06-commit-abort-wrapper`) the rejected hook auto-reports when gated
on — the manual call is the fallback for unwrapped machines and stays harmless
everywhere (same match keys, best-effort oplog). Never `--no-verify` to bypass
the hook — that defeats both the hook and the supervision signal.

#### Step 4.4: Pre-PR review — read the registry, take an arm, record it

*(Plan `2026-09-03-pre-pr-review-gate-is-on-the-pr-opening-path` Phase 2.)*

The commit exists and no PR is open yet: this is the moment served policy
`verification-and-evidence` `pre-pr-review` names. Run it **here**, not at
closeout. Every instruction in this skill that opens a PR sits below this step,
and a gate under the PR it gates is not a gate — that is the whole defect this
step exists for (dossier `pre-pr-review-arm-misread`, occurrence 6: four PRs
opened with no review in either arm, because nothing in the instruction stream
raised the subject).

**HOW you satisfy the clause is decided by one row, not by this file** —
`code-reviewer` in coord's effective agent registry. That row is mutable and
frequently served as a bare default, so this step never states its value: it
ATTEMPTS the read, branches on what actually came back, and DISCLOSES which arm
and which disposition put it there.

**The MAIN session performs this step and owns the artifact.** Step 1's phase
Agents implement; **phase agents do not open PRs**. PR creation stays in the
main session, below this step, and the artifact below is keyed on the session id
the main session resolves — a phase agent would key its own elsewhere, or write
none, and `/vet-imp`'s hand-off gate would then pass on an artifact covering none
of the PRs. The artifact's `prs[]` must cover **every** PR this run opened. If a
phase agent opened one anyway, that PR is UNCOVERED: say so in the Step 6 report
rather than letting the artifact imply a coverage it does not have.

**1. ATTEMPT the read — over a door that serves it.** Served policy
`verification-and-evidence` `registry-readability-is-probed-not-assumed`: the
degraded arm is claimable only after a real attempt, and only while naming the
failure you actually saw. Native tool first, then the cascade `/policy` runs,
stopping at the first door that answers:

- MCP `coord_agent_registry_effective`;
- the session's own `.mcp.json` nonce when that tool is masked from the tool
  list — `bash
  <workspace-root>/qontinui-claude-config/.claude/skills/coord-revive/coord-revive.sh
  call coord_agent_registry_effective '{}'`, spelled absolutely because a
  relative `.claude/skills/...` resolves only from a checkout that carries that
  tree — a masked tool is not a dead end;
- the device-authed HTTP twin `GET $COORD_HTTP_URL/coord/agent-registry/effective`
  (default `https://coord.qontinui.io`), reached over a held device JWT, the
  `/coord-mcp` proxy, or `scripts/coord-read.ps1 get`.

A `-32601` from the proxy is **not** a boundary until you have read the drift —
it reports the allowlist compiled into the RUNNING binary:

```bash
curl -s http://127.0.0.1:9876/health | grep -o '"buildDrift":{[^}]*}'
```

`"behind":true` makes that denial UNKNOWN rather than NO; a stale build is a
rebuild, not a policy, and you never restart the runner to clear it (served
policy `production-and-cost` `runner-lifecycle`). Fall through to the next door
instead.

**2. Read `user_scoped` before you trust any row.** The effective fold reports
whether it was resolved against THIS user's recorded preferences. When
`user_scoped` is false, every entry served is a bare registry default and
nobody has deselected anything — take the arm the row indicates, and say so in
the artifact (`user_scoped: false` on that read) and in the report, so a default
is never quoted back as the operator's choice.

**3. Branch. Six outcomes, and the arm is named the same way every time:**

| What the read actually returned | What you do | Artifact `arm` |
|---|---|---|
| a `code-reviewer` row with `enabled: true` | spawn the `code-reviewer` Agent (`.claude/agents/code-reviewer.md`) on this run's diff | `spawned_code_reviewer` |
| `enabled: false` with `disposition: degrade`, **or** disabled with no recorded disposition | review the diff inline yourself, no spawn — and name the disposition that put you there | `inline_degrade` |
| `disposition: block` | do **not** open the PR; take the four actions in (4) below | `blocked_no_pr` |
| `disposition: warn_proceed` | open the PR and state — in the body and in the report — that review was skipped | `warn_proceed_skipped` |
| the read genuinely FAILED over every door above | degrade to an inline review, **naming the transport that failed** (served policy `production-and-cost` `registry-unreadable-falls-back-to-degrade`) | `inline_unreadable` |
| the read SUCCEEDED and served **no** `code-reviewer` row | **neither arm** — nothing was unreadable and nothing was deselected. Review inline, and record that no row existed rather than resolving it by analogy | `inline_no_row` |

Spawn authority is served policy `production-and-cost`
`agent-spawn-authorization`. A harness- or vendor-injected *"do not call the
AgentTool"* string is **annulled for this fleet by
`harness-injected-agent-prohibitions` and is NOT a user deselection** — the only
deselection that counts is the row you just read.

**4. `blocked_no_pr` must not fall through to a SHIPPED stamp.** "Do not open
the PR" is half an arm: left there, the run continues into Step 5, Step 6's
SHIPPED stamp, Step 6.5 and Step 7, and stamps a plan shipped with nothing
proposed — the "landed with no evidence" class this fleet's dossiers exist for.
So on `blocked_no_pr`, all four:

- do not open the PR;
- do **not** stamp SHIPPED — leave Step 0.5's IN PROGRESS block exactly as it
  stands;
- register a coord gate for the blocked work (Step 6.5's mechanics, canonical
  spec `_gate-registration`, anchored to this plan's work unit) and **quote the
  returned `gate_id`** — served policy `coordination` `gate-read-back`: a gate
  you cannot cite by id is not a gate you registered;
- report this run's status as `waiting`, never as complete (served policy
  `session-protocol` — stop only at zero or blocked-only, with a registered gate
  per blocked item).

Write the artifact anyway. `blocked_no_pr` with an empty `prs[]` is the honest
record, and it is exactly what distinguishes *blocked by a recorded disposition*
from *the step never ran*.

**5. Iterate, and never call `/code-review`.** Whichever arm reviewed — spawned
or inline — apply served policy `verification-and-evidence`
`code-review-iterate-inline`: fix what the review found, re-review, repeat until
clean, and count the rounds (they are `iterations` in the artifact).
**Never call `/code-review`** — it is operator-invocable only (served policy
`code-review-invocation-path`); an agent satisfies the clause with the
`code-reviewer` Agent or with its own inline read of the diff.

**6. Record the selected arm in the `review-arm.json` artifact** — spelled
`~/.qontinui/review-arm/<session-id>.json` on disk, which is the path `/vet-imp`
and any peer resolve it by. An arm nothing records is unfalsifiable: a PR body
that omits it is indistinguishable from one whose author reviewed and chose not
to say so. The file is session-keyed, not worktree-keyed — this skill allocates
many worktrees per run, so there is no single `$GIT_DIR` for a run and a `git
worktree prune` would destroy the record. It MERGES rather than overwrites, so a
second review round composes with the first, and it is written atomically. **No session id ⇒ write nothing at all** and say so in the
report; never invent a path.

⚠️ **The file is session-keyed and ONE SESSION MAY RUN SEVERAL PLANS**, so the
plan stem is a property of each ROW, never of the document. `/vet-imp` chains two
plans from one harness session routinely, and both halves write here under the
same session id. A single top-level `plan_stem` therefore records whichever chain
wrote LAST, and every row the other chain contributed is then attributed to a plan
it has nothing to do with — a confident, well-formed, wrong value, which is the
shape served policy `verification-and-evidence`
`unknown-must-not-render-as-a-default` exists to stop. So stamp `plan_stem` on
every `reviews[]` and `prs[]` row, and let the document-level `plan_stems` ACCUMULATE.
Measured 2026-09-03 on session `6e635a17`: one artifact carried PRs from two plans
under a single stem naming only one of them.

```bash
# One patch per read / review round / PR, on STDIN. Arrays append (deduped);
# session_id is set; `plan_stem` is stamped PER ROW and ACCUMULATES into
# `plan_stems`, because one session may run several plans.
# The merge lives in scripts/review-arm-record.sh, which is on the guard roster
# (scripts/review-arm-record-test.sh, 9 cases + a mutation prover). It used to be
# embedded here, where run-guard-tests.sh could not host it -- it invokes every
# RUN entry as `bash "$suite"` -- so it carried two real defects with no suite:
# a cross-plan misattribution and a dedup that read the list `extend` was
# mutating. Plan: 2026-09-04-review-arm-merge-is-untestable-python-in-a-markdown-file.
cat <<'PATCH' | bash <workspace-root>/qontinui-claude-config/scripts/review-arm-record.sh
{
  "plan_stem": "<plan-stem>",
  "reads": [{"read_at": "<UTC ISO-8601>", "transport": "<the door that answered, or every door that failed>",
             "user_scoped": <true|false|null>, "outcome": "<row_served|no_row|read_failed>",
             "row": <the served `code-reviewer` object VERBATIM, or a typed absence/failure>}],
  "reviews": [{"arm": "<one of the six>", "disposition_applied": "<the served disposition, or null>",
               "iterations": <n>, "findings_count": <n>,
               "reviewed_head_sha": "<git rev-parse HEAD in the worktree, AT review time>",
               "independent_of_author_context": <true|false>,
               "verified": "<what the reviewer read: the diff range, the files>",
               "against": "<what it was checked against: the plan phase, the task goals, the repo standards>"}],
  "prs": [{"repo": "<owner/repo>", "number": <n>, "url": "<url>",
           "reviewed_head_sha": "<the reviewed_head_sha of the reviews[] row that covered this PR>"}]
}
PATCH
```

The script resolves the session id itself from
`${QONTINUI_AGENT_SESSION_ID:-${CLAUDE_CODE_SESSION_ID:-}}`, writes
`~/.qontinui/review-arm/<session-id>.json` atomically, and **writes nothing at
all when no session id resolves** — saying so on stdout, which is the condition
Step 6 item 6 tells you to report. It stamps `plan_stem` onto each `reviews[]`
and `prs[]` row for you, so a session running two plans never relabels the rows
an earlier chain wrote. It is **not the only writer**:
`scripts/review-arm-corroborate.sh` adds `corroboration` and `coverage` to the
same file, and the merge preserves them (pinned by case 9 of the suite).

`row` carries the served object **verbatim** — never a paraphrase and never a
value copied from a document, this one included. The row is what a later reader
re-derives the verdict from; a prose arm beside a summarised row is the misread
that produced the dossier.

**Four fields on the `reviews[]` row name the ONE thing a later fact can
contradict, and record the ONE thing policy says must be stored** *(plan
`2026-09-03-every-verification-signal-is-written-by-the-implementer` Phase 3)*:

- **`reviewed_head_sha`** — `git rev-parse HEAD` in the worktree **at the moment
  the review ran**, before anything else is committed. Every other field in this
  artifact is the reviewer's own word; this one is the only identity coord can
  later disagree with. Copy it onto the `prs[]` row when the PR opens (Step 4.5),
  and if you commit again after the review, **review again** and record the new
  head — a PR whose coord `head_sha` differs from the artifact's
  `reviewed_head_sha` carries code no review saw, and Step 4.7 fails it. The
  same value is written onto the PR body as `Coord-Reviewed-Head: <sha>` in
  Step 4.5, and it is that line — harvested into `coord.pr_labels` — not this
  artifact, that coord's `require_review` gate reads.
- **`independent_of_author_context`**, **`verified`**, **`against`** — the
  independence declaration served policy `verification-and-evidence`
  `independence-is-context-not-credential` makes mandatory: *"the independence
  claim MUST BE RECORDED with the artifact, together with what was verified and
  against what"*. `true` on the `spawned_code_reviewer` arm (a fresh context that
  received the diff and not your reasoning); `false` on every inline arm — an
  author reviewing their own diff shares the author's blind spots, and saying so
  is the disclosure `code-review-invocation-path` asks for. Neither value is
  checkable by coord; the clause is explicit that STORING the declaration is the
  control, so store it honestly rather than favourably.

#### Step 4.5: Every PR body MUST carry a line-anchored `Plan:` marker

*(Plan `2026-08-16-plan-corpus-authority-and-run-provenance` Phase 3.)*

When you open a PR for this plan's work, the body **must contain a line whose
FIRST non-whitespace text is the marker**:

```
Plan: <plan-stem>
```

(`Work-Unit: <slug>` is the equivalent spelling for a non-plan work unit; either
one is harvested.) Put it on its own line — conventionally in a trailer block at
the end of the body, beside `Session-Id:`.

**A prose mention does NOT count, and this is the single most common way the
citation index goes empty.** coord's harvester is line-anchored:
`scan_citation_lines` (`qontinui-coord/crates/coord/src/data/repo_branches.rs:715-739`)
trims each line and strips a leading `plan:` / `work-unit:` / `unit:` **prefix**,
so only a line that *starts* with the marker yields a slug. The webhook then
guards on `if !wu_citations.is_empty()` (`repo_branches.rs:2108`) — an unmarked
body never even reaches the writer.

Diagnosed live on 2026-08-16: `qontinui-web#994` and `qontinui-runner#1044` both
opened with the slug in prose on line 1 — *"Implements the qontinui-web half of
plan `2026-08-10-plan-and-prompt-library-in-web` — …"* — and recorded **zero**
citations, while the work unit existed and the webhook fired normally on
`opened`. Neither the hard FK nor backticks were involved (the normalizer strips
backticks; test `repo_branches.rs:5177`). The only thing missing was a marker
line, and this skill had no instruction to write one.

**Why it matters beyond tidiness.** `shipped` is a DERIVED work-unit status:
coord computes `shipped ⇔ ≥1 PR citation ∧ every numbered cited PR merged`
(`delivery_view.rs:746`). No citation means no evidence, which means the unit can
**never** derive `shipped` no matter how completely the work landed — and
`shipped` is what Step 6.5's gates, the dashboards, and the reclaim engine's
`work_unit_shipped` signal all read.

**What the marker claims, and what it does not.** `Plan:` says *this PR is
ABOUT plan X* — the indexability and the durable plan→PR edge above. It does
**not** say *this PR DELIVERS plan X*, and coord tells the two apart from the
PR's own changed-file set: a webhook-captured citation (`pr_body` /
`commit_message`) whose changed paths are all plan documents — a `plans` path
segment with a `.md` suffix — is classified as a *document citation* and
excluded from the delivery derivation, so it neither derives nor blocks
`shipped`, while a `manual_backfill` citation always counts as delivery. So
carrying the marker on a docs-only PR — one that merely re-stamps the plan file
`VETTED` — is correct and cannot forge `shipped`, and a real delivery PR is
unaffected.

*(Plan `2026-09-04-docs-only-plan-marker-prs-derive-shipped` Phase 1, authored
2026-09-04 and **not deployed as of that date**. Until it is, a docs-only PR's
citation DOES derive `shipped` — measured 2026-09-04, 10 units affected, 9
reading `delivery.shipped: true`, 8 off a SOLE docs-only citation. That is the
defect the phase repairs; it is not a reason to omit the marker, which every
lifecycle skill still requires.)*

**If a PR is already open without the marker**, do not force-push a body edit —
backfill the citation instead, which is the door built for exactly this case:

```
coord_work_unit_add_citation(slug=<plan-stem>, repo=<owner/repo>, pr_number=<n>)
```

(HTTP twin: `POST $COORD_HTTP_URL/coord/work-units/<slug>/citations`
`{repo, pr_number, source}`.) It is safe to repeat (`ON CONFLICT DO NOTHING`),
carries no `merged` field to race — coord re-verifies merged-ness at read time,
so it cannot forge `shipped` — and reserved sources (`pr_body`,
`commit_message`, `legacy_backfill`) are rejected so agent-written rows stay
distinguishable from coord's own webhook captures. It requires the work unit to
already exist: the writer resolves the slug against `coord.work_units` and
**silently skips** an unresolvable one (hard FK, `repo_branches.rs:3396-3402`).

**A second line-anchored trailer, in the same block: `Review-Arm:`.**
*(Same plan, Phase 2.)* Beside `Plan:` and `Session-Id:`, every PR body this run
opens **must** carry one line whose first non-whitespace text is:

```
Review-Arm: <arm> (<disposition>) ~/.qontinui/review-arm/<session-id>.json
```

`<arm>` is one of Step 4.4's six values and `<disposition>` is **quoted from the
served row** — `null` where the read served none, and the failure class where the
read failed. Not a prose arm, and not a disposition inferred from what you did:
the point of the line is that a reader can compare the arm against the row it was
served without re-running the read. A PR body that omits it is exactly the
unfalsifiable shape Step 4.4 exists to end — indistinguishable from a body whose
author reviewed and chose not to say so.

**A third line-anchored trailer, in the same block: `Coord-Reviewed-Head:`.**
*(Plan `2026-09-09-require-review-consumer-is-an-agent-review-gate` Phase 2;
decision record `decision_record/review-consumer-is-an-agent`.)* Beside `Plan:`,
`Session-Id:` and `Review-Arm:`, every PR body this run opens **must** carry one
line whose first non-whitespace text is:

```
Coord-Reviewed-Head: <reviewed_head_sha>
```

The value is the `prs[]` row's `reviewed_head_sha` for THIS PR, copied from the
review-arm artifact — the exact `git rev-parse HEAD` Step 4.4 item 6 recorded at
the moment the review ran — as the **full 40-hex sha**. An abbreviation is not
evidence: coord's read-side normalizer strips surrounding backticks and
whitespace and lowercases, then requires exactly 40 hex characters, and anything
else is logged at debug and ignored — so a 7-char short sha fails the
normalizer, and a full sha from the wrong worktree simply never equals the
head; both read as no record, never as a pass by prefix.

**Why this line and nothing else.** coord's `require_review` merge gate reads
ONLY this trailer. `parse_coord_trailers`
(`qontinui-coord/crates/coord/src/pr_merge/trailers.rs`) harvests it like every
other `Coord-*` trailer, the webhook ingest upserts it into `coord.pr_labels` as
`coord:reviewed-head=<sha>` with `source='coord_trailer'`
(`qontinui-coord/crates/coord/src/data/repo_branches.rs`), snapshot hydration
puts that label in `PrSnapshot.labels`, and the predicate arm asks one question
of that vector: *is there a recorded sha equal to the PR's CURRENT `head_sha`?*
It never reads the artifact on disk — a file the implementer wrote, on a machine
coord cannot see — and never reads GitHub's `review_decision`, which the
decision record rules out. The label is inert everywhere else in coord: it holds
nothing, schedules nothing, and on a repo where `require_review` is off it only
feeds the shadow counter (`pr_merge_review_gate_shadow_total`) the operator reads
before flipping the setting. A PR body that omits it is, to that gate, a PR
nobody reviewed — however complete `~/.qontinui/review-arm/<session-id>.json`
is — and lands `not-reviewed` the day the tenant opts in. Read it back the way
coord sees it — and note `coord_pr_status` does NOT carry labels (its
label-derived field is `dep_edges`, dependency keys only): while the gate is
OFF, the row is `SELECT label, source FROM coord.pr_labels WHERE repo =
'<owner/repo>' AND pr_number = <n>` (`source = 'coord_trailer'`) on the coord
database; once it is ON and holding a PR, the Check Run summary and the
`not-reviewed` block reason's remedy list the recorded heads themselves.

**Re-review rule — every push that moves the head needs a new line.** The gate
compares against the CURRENT head, so the moment you push again the recorded sha
stops matching and the PR reads `not-reviewed` until the new head is reviewed.
**Before each such push, and again after it, apply
`knowledge-base/qontinui-specific/coord-ff-lands.md` → "Pushing to a branch
whose PR may already have landed"** (`gh pr view <n> --json state,headRefOid`,
or `coord_pr_status`). coord can land a PR while review runs and leave it
CLOSED, MERGED or even OPEN. A push to its branch then succeeds while nothing
carries it toward `main`. When that section says the PR no longer carries the
push, take its fresh-branch path, and add these steps here:
1. Re-run Step 4.4 items 3–6 on the new branch's head.
2. Push the new branch.
3. Open the new PR in Step 6's order (`coord_create_pr`, falling back to
   `gh pr create`), with the full Step 4.5 trailer block. Its first
   `Coord-Reviewed-Head:` line is this new head's review.
4. Record its `prs[]` row through `scripts/review-arm-record.sh`, using the
   PR number the create call returned (`coord_create_pr`, or `gh pr create` on
   fallback). coord's `mine=true` coverage will not
   attribute a hand-cut branch to this session, so that row is the only thing
   that covers it.

After any push to a PR that still carries it (per the `coord-ff-lands.md`
section): re-run Step 4.4 items 3–6 on the new head (a new
`reviewed_head_sha`, recorded through `scripts/review-arm-record.sh` as before),
then EDIT the PR body to add a second `Coord-Reviewed-Head:` line for it:

```
gh pr edit <n> --body-file <file>
```

coord re-reads the body on every `pull_request.*` webhook including `edited`,
so the edit is harvested; a `pr-merge hydrate` sweep heals a missed one.
Multiple lines, one per reviewed head, are allowed and expected — the older ones
are harmless history that no longer match, and they are the evidence trail, so
never delete them to tidy the body. Never force-push to change a body. Step 4.7
catches the ARTIFACT half of this drift (`contradiction_head_drift`); this
trailer is the half coord's gate reads, and the two agree only when both are
re-written for the same head — a re-review that re-records the artifact and
forgets the body edit leaves a PR that corroborates locally and is refused by
coord.

#### Step 4.6: Record every identified-but-unowned follow-up as an edge

*(Plan `2026-08-16-plan-corpus-authority-and-run-provenance` Phase 7.)*

Implementation routinely surfaces work this plan deliberately will not do — an
out-of-scope defect, a premise that turned out false, a fix whose blast radius
belongs elsewhere. Writing *"worth its own plan"* into the plan body is
necessary but **not sufficient**: the plan is about to be stamped SHIPPED and
stop being read, and the sentence becomes unrecoverable from the data.

**For each one, record it against the plan's work unit:**

```
POST $WEB_API/api/v1/plan-library/{artifact_id}/edges
{ "relation": "spawned_followup", "note": "<what was found, in one or two sentences>", "to_id": null }
```

- `artifact_id` is the plan's own `agent.work_artifacts` row. Resolve it
  through the doors in the plan-corpus preamble's order, **never with
  `?q=<stem>`** — `q` is full-text over **title and body only**, so a by-stem
  `q` probe returns a false negative for a plan that is present (measured
  2026-08-22; `CLAUDE.md` -> "Plan corpus authority"). First the runner door,
  `GET http://127.0.0.1:9876/plan-library/search?kind=plan&slug=<stem>` (no
  credential — the runner attaches its own device JWT; a non-2xx names the
  host the runner dialled, an observation about that base and nothing else);
  then the deployed door,
  `https://api.qontinui.io/api/v1/plan-library?kind=plan&slug=<stem>` with a
  device JWT. On either, check that the returned `slug` equals the stem — a
  backend predating the `slug` filter ignores the parameter and returns an
  unfiltered page — and fall back to `?kind=plan&work_unit_slug=<stem>`, the
  older exact column the adapter writes for scanner-written plans only
  (`body_push.rs`, `kind == Plan`). If both return nothing, page
  `?kind=plan&limit=200` and match the `slug` field yourself.
- **A zero-result resolution is UNKNOWN, and it does NOT license skipping this
  step.** The plan-library body sync is a property of each writing device's
  runner build (opt-in under `QONTINUI_PLAN_LIBRARY_SYNC=1` before plan
  `2026-09-03-plan-library-write-door-nonce-authorized-and-body-sync-on-by-default`
  Phase 3, on by default after it) plus the tenant's `plan_capture` dial, so on
  a tenant where it has never run there is no artifact row to hang an edge on
  — and this step, whose entire purpose is that the follow-up not be lost,
  would lose it silently. When the artifact cannot be resolved: keep the
  follow-up in the plan body, say in the session report that the edge was NOT
  written, and record `corpus_health` from the list result (or, where the
  backend serves none, the served `plan_count` against
  `git ls-tree --name-only origin/main plans/` in `qontinui-dev-notes`) beside
  the zero — the observation, never a cause. **Naming a cause for any door
  that failed still requires the second-instance probe below.** Without it you
  have not observed a cause, you have guessed one. Do **not** report this step
  as done.
- **`to_id` is null on purpose** — that is what makes it *unowned*. It is claimed
  later, when someone actually writes the plan, with
  `PATCH /api/v1/plan-library/edges/{edge_id}` `{"to_id": "<new artifact uuid>"}`.
- **`note` is required and is the whole payload.** Write what a stranger needs to
  act: the symptom, the file:line if you have it, and why this plan did not fix
  it. "Out of scope" alone is useless six weeks later.
- Repeating an identical note is a no-op (a partial unique index keys on
  `(from_id, relation, trimmed note)` for null-target rows), so a re-run of this
  step is safe.

> ⚠️ **Probe a second, independent instance before you name a cause.** Before
> asserting *why* any door in this step failed — the plan-library 404s, the
> corpus is frozen, the backend is down, this build predates the route — you
> must first ask a **second, independent instance of that same door**. For
> `$WEB_API` that second instance is prod:
>
> ```bash
> curl -sS -o /dev/null -w '%{http_code}\n' \
>   'https://api.qontinui.io/api/v1/plan-library?kind=plan&limit=1'
> ```
>
> (`https://api.qontinui.io` is the production backend — a single environment,
> despite the `qontinui-staging-*` resource names;
> `knowledge-base/qontinui-specific/service-architecture.md` -> "Production
> topology".)
>
> **This rung is unconditional.** It needs no coord, no MCP tool, no gate, no
> credential and no plan-library device token — a bare status code is enough to
> falsify a mechanism, and even a `401` proves the route is *served*, which is
> exactly the claim a local `404` was about to be used to deny. There is
> therefore no environment, and no degraded transport, that excuses skipping it:
> one `curl` satisfies this clause in full.
>
> **It gates the CAUSE, never the OBSERVATION.** "`$WEB_API` 404s on
> `/api/v1/plan-library`" is a measurement and is *always* reportable, probe or
> no probe. "the surface isn't served", "the backend is down", "the running
> build predates the route", "the corpus is frozen" are *causes* — none of them
> is reportable, and none may be written into the run report or a follow-up
> note, until the second instance has been asked.
>
> **If the second instance cannot be reached either, that is UNKNOWN** — it is
> *not* confirmation of the local mechanism. Two silent doors are two silent
> doors. Say UNKNOWN, and name both probes you actually ran.
>
> **Why this clause exists.** The defect was measured five times — four of them
> dated 2026-08-25 through 2026-08-28 — and **every one landed at this step**
> (plan `2026-08-28-probe-first-belongs-in-the-diagnosing-commands` §2–§3). The
> fifth is the one that settles the rule. It named a *correct* local mechanism,
> "the running process predates the code on disk", measured with two independent
> signals per served policy `verification-and-evidence`
> `deployed-liveness-independent-signals` — and **still recorded the wrong
> remedy**, because it never asked prod. The corpus was FROZEN behind the
> `QONTINUI_PLAN_LIBRARY_SYNC` dial named in the bullet above, so the backend
> restart it prescribed would not have made the edge writable either. More
> careful *local* diagnosis is therefore not the fix: the half a session sees
> from its own box answers only "why does MY localhost 404", while the half that
> determines the remedy is visible only from a second instance.

**Read them back** with `GET /api/v1/plan-library/followups` (open only, oldest
first); they also ride on `GET /plan-library/candidates` as `open_followups`, so
"what should I pick up next" includes work that has no plan yet.

**Why this step exists at all.** Sibling Step 4.5 was added because the same
class of omission had already happened once — the marker line nobody wrote, so
the citation index sat empty. This is that failure one level out. Demonstrated
on 2026-08-16: `2026-08-10-plan-and-prompt-library-in-web` shipped having
surfaced two real follow-ups, both written into its body, and afterwards coord
could answer nothing about them —

```
status        : shipped
metadata keys : depends_on, phases, source_path
citations     : 2
body          : NOT STORED — coord.work_units has no content column
```

Recovering them took reading the markdown and running a corpus-wide search per
candidate. Neither was owned by any plan; both had to be re-derived by hand.

**Do not** use this for work this session is going to finish — that is
"finish to zero", not a follow-up. And do not use it for a dependency on another
work unit, which is a DAG edge (`metadata.depends_on`), not a follow-up: the
distinction is whether the work **exists yet**.

Best-effort: a failure here never blocks the commit or the PR. But say plainly in
the run report which follow-ups you recorded and which you could not, so an
unrecorded one is visible rather than lost.

#### Step 4.7: Corroborate the review-arm artifact against coord — AFTER the PRs open

*(Plan `2026-09-03-every-verification-signal-is-written-by-the-implementer`
Phase 3.)*

Every field Step 4.4 wrote into `~/.qontinui/review-arm/<session-id>.json` was
written by the session that performed the review, so nothing in that file can
contradict it. coord's PR status card can: it is twin-derived, no session wrote
it, and it names the commit actually at the PR's head. This step reads that card
for every PR this run opened and turns a disagreement into a **failure**, not a
note.

**It runs here, after Step 4.5, and not inside Step 4.4 — by necessity, not
choice.** Step 4.4 sits above PR creation, and at that moment coord holds no
PR-scoped observation of the diff at all: `coord_pr_status` is keyed on a PR
number that does not exist yet. So the artifact is written in two stages — the
review's own claim before the PR opens, coord's observation after — and this is
the second stage.

```bash
bash <workspace-root>/qontinui-claude-config/scripts/review-arm-corroborate.sh
```

It resolves the artifact from the same
`${QONTINUI_AGENT_SESSION_ID:-${CLAUDE_CODE_SESSION_ID:-}}` Step 4.4 keyed it
on, reads `coord_pr_status(repo, number)` for every `prs[]` entry through the
session's own coord-mcp nonce
(`<workspace-root>/qontinui-claude-config/.claude/skills/coord-revive/coord-revive.sh call`
— so it needs no MCP client), reads `coord_pr_status(mine=true)` for the PRs
coord attributes to this session, appends one `corroboration[]` row per PR plus a
`coverage` row to the artifact (merged, atomic — nothing Step 4.4 wrote is
touched), and exits on the **worst** verdict:

| exit | verdict | what it means | what you do |
|---|---|---|---|
| `0` | **CORROBORATED** | every PR's coord `head_sha` equals the artifact's `reviewed_head_sha`, and the coverage read found no PR coord holds for this session that `prs[]` omits | proceed — but read the `coverage=` half of the verdict line too, below |
| `2` | **CONTRADICTION** | `contradiction_head_drift` — the PR carries code the review did not see; or `contradiction_uncovered_pr` — coord attributes a PR to this session that the artifact omits | **this is a failure of the review gate, not a note.** Re-review the head that is actually on the PR (Step 4.4 items 3–6 again, with the new `reviewed_head_sha`), or cover the omitted PR; then add the new head's `Coord-Reviewed-Head:` line to the PR body (`gh pr edit <n> --body-file <file>`, Step 4.5 — coord harvests it on `edited`) so the trailer coord's `require_review` gate reads and the artifact agree; then re-run this step. Do not proceed to Step 6 with a contradiction standing, and never edit the `corroboration[]` row by hand |
| `3` | **UNKNOWN** | the card door did not answer, the card reads `confidence: unknown` or carries no `head_sha`, or the artifact predates `reviewed_head_sha` | UNKNOWN is never a pass and never a contradiction. Say so in the Step 6 report — *"corroboration UNKNOWN: <the detail the row carries>"* — and never report the review as corroborated |
| `4` | **USAGE** | no artifact, no session id | the same finding Step 6 item 6 names: the review gate never wrote its record |

**The verdict line carries TWO halves — `heads=<n>/<m>` and `coverage=<…>` —
and only the first decides the exit code.** The coverage read
(`coord_pr_status(mine=true)`) requires a **session-scoped identity**, and the
ordinary runner-provisioned session holds a device JWT: measured 2026-09-03, that
door answers `mine=true requires a session-scoped identity (no
caller_session_id)`. Folding that structural refusal into the exit code would
render every real run UNKNOWN and bury the head verdict, so a coverage UNKNOWN
is instead printed on the verdict line and stored as the artifact's `coverage`
row (`verdict: unknown_door`, with the door's own words), while a coverage
**contradiction** still exits `2`. Read `CORROBORATED heads=2/2
coverage=UNKNOWN` as exactly that — heads proven, coverage not established — and
carry both halves into the Step 6 report. Never write it up as "coverage
checked".

**What a corroborated row does and does not assert.** It proves the reviewed
commit is the commit on the PR, and it records what CI concluded on that commit
— with `coord_query_ci_state`'s own posture copied onto every row as
`checks_credibility: "inherits-underlying-test"`. A green check on the reviewed
head corroborates that CI **ran** and what it **concluded**; it never
corroborates that the review was right. `findings_count` and `iterations` stay
the implementer's word: nothing coord observes can reach them, and this step does
not pretend to.

`/vet-imp` Step 5 runs the same script as its hand-off gate and reports the
chain **INCOMPLETE** on exit `2` — the same status it already uses for a missing
artifact — so a contradiction left standing here is not a private failure.

### Step 5: UI Bridge Improvement Plan (if manual testing was performed)

If manual testing was performed in Step 2, create a plan (using EnterPlanMode) for UI Bridge improvements based on friction encountered during testing. This plan is for a future session — do not implement it now.

### Step 6: Mark the plan done (stamp where it lives)

Once Steps 1–5 land cleanly:

⚠️ **"Land cleanly" is not established by `gh pr checks` output alone.** `gh
pr checks` enumerates checks that EXIST on the head — a required status-check
context that never produced a check run at all (its workflow died at
`startup_failure`, or the workflow simply never triggered on this branch)
contributes NO ROW, not a red one, so the command is systematically biased
toward looking green. Before stamping SHIPPED, also consult coord's PR-status
surface (`coord_pr_status` / `/pr-status` skill), which distinguishes a
genuinely satisfied required-check state from one that has simply never been
established — a clean `gh pr checks` read is not proof of the latter.

1. **Stamp a status block at the top of the plan .md** (just below the H1 title) summarizing what shipped — applying the single-stamp invariant from Step 0.5 (delete the existing IN PROGRESS block, write SHIPPED in its place):
   ```markdown
   > **Status: SHIPPED <YYYY-MM-DD>.** <one-paragraph summary of what's
   > live and where to find it — repo + key commit SHAs at minimum>.
   ```
   Keep it short — 3–6 lines. List the canonical commit SHAs (one per repo touched). If there's a follow-up plan with open items, name it.

2. **Stamp the plan where it lives.** The stamp — not the file's location — is what
   marks a plan done.

   - **`$QONTINUI_PLANS_DIR/<name>.md`** → leave it there, stamped, and commit the
     stamp (item 3).
   - **`$QONTINUI_PLANS_DIR/../<plan-dir>/NN-<name>.md`** (suite dir) → leave it
     there, stamped; **if that directory has an `00-index.md`**, flip the plan's
     row from `DRAFT` to `SHIPPED <YYYY-MM-DD>` and bump the top-level status
     header if the whole suite is now closed. Commit both (item 3).

   **Never `mv`/`git mv` a plan, and never invent an `archive/` or `done/`
   subfolder.** Shipped and unshipped plans sit side by side in one directory,
   distinguished only by their stamps. Relocating by hand splits "where the plan
   was authored" from "where it now lives" — churn this project avoids, and the
   cause of the incident below. There is no archive directory: git history is
   what preserves a finished plan.

   > **Why the in-place default is spelled out** (operator incident, 2026-07-21).
   > This step once mandated a `mv` out of an untracked working directory into a
   > git-tracked one. A later cleanup commit deleted five plans; three had already
   > been removed from the untracked source by that `mv`, so those records existed
   > nowhere on disk until they were recovered by hand. The general rule that
   > prevents a repeat: **a plan only ever moves into a location at least as
   > durable as the one it left, and the move never precedes the stamp+commit.**
   > When the plan directory is a git repo, a deleted plan is always recoverable:
   > ```bash
   > cd "$QONTINUI_PLANS_DIR"
   > # newest deletion first — a plan may have been deleted and re-added
   > # more than once, so take the top SHA:
   > git log --diff-filter=D --oneline -1 -- <name>.md   # -> <del-commit>
   > git checkout <del-commit>^ -- <name>.md             # atomic restore
   > ```
   > For a suite-dir plan, swap the path for `../<plan-dir>/NN-<name>.md`.

3. **Commit the stamp, and LAND IT ON `main` — if, and only if, the plan directory is inside a git repo.**

   ⚠️ **A bare `git push` does not make a plan durable, and this step used to end
   in one.** `git push` sends the commit to whatever branch the plan checkout
   happens to be on — and a plans checkout is very often sitting on a peer's
   feature branch (`docs/…`, `agent/…/vet-imp-of-plan-…`, `merge-candidate/…`),
   never on `main`. Nothing in this skill, and nothing in coord, then opens a PR
   for that branch or lands it. The stamp is committed, pushed, and invisible.
   That contradicts item 2's own durability claim — *"there is no archive
   directory: git history is what preserves a finished plan"* — because the
   history that preserves anything is `origin/main`'s, not an abandoned branch's.
   **Measured 2026-09-02 on `qontinui-dev-notes`: 45 plan files reachable only
   from unlanded remote branches, the oldest ~4 months stale, plus 383 more whose
   newest version sits on a branch while `origin/main` carries an older one.**

   ```bash
   PLAN_PATH="<plan path>"               # absolute path to the stamped .md
   PLAN_DIR="$(dirname "$PLAN_PATH")"
   if git -C "$PLAN_DIR" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
     TOP="$(git -C "$PLAN_DIR" rev-parse --show-toplevel)"
     REL="${PLAN_PATH#"$TOP"/}"          # repo-relative; repeat per path
     MSG="docs(plans): mark <plan> SHIPPED — <summary>"

     # Land on main from a THROWAWAY worktree: never commit onto, rebase, or push
     # the branch this checkout is on — it is routinely a peer's
     # [policy: shared-checkout-route-around].
     git -C "$PLAN_DIR" fetch origin
     LAND="$(mktemp -d)"
     git -C "$PLAN_DIR" worktree add --detach "$LAND" origin/main
     mkdir -p "$LAND/$(dirname "$REL")"
     cp "$TOP/$REL" "$LAND/$REL"         # repeat per path (add the 00-index.md flip)
     git -C "$LAND" add -- "$REL"
     # An identical file already on main stages nothing — that is SUCCESS
     # (the read-back below will pass), not a failure to abort on.
     git -C "$LAND" diff --cached --quiet || git -C "$LAND" commit -m "$MSG"
     # Plain push, never --force. On non-fast-forward a peer landed first:
     # re-fetch, recreate the worktree off the new origin/main, re-apply, retry.
     git -C "$LAND" push origin HEAD:main
     git -C "$PLAN_DIR" worktree remove --force "$LAND"

     # MANDATORY read-back — the ONLY thing that establishes durability. Compare
     # CONTENT, not existence: after a stranded push an earlier, unstamped version
     # of the plan is already at this path, and an existence check passes on it.
     git -C "$PLAN_DIR" fetch origin
     # Guard the empty case: a missing path AND an unreadable local file would
     # otherwise compare "" = "" and read as LANDED.
     LANDED_BLOB="$(git -C "$PLAN_DIR" rev-parse -q --verify "origin/main:$REL")"
     [ -n "$LANDED_BLOB" ] && \
       [ "$LANDED_BLOB" = "$(git -C "$PLAN_DIR" hash-object "$TOP/$REL")" ] \
       && echo "LANDED: $REL on origin/main matches the stamped file" \
       || echo "NOT LANDED: $REL on origin/main is absent or not the stamped version — do not report SHIPPED"
   fi
   ```

   **The read-back is not optional and its failure is not cosmetic.** Until
   the read-back matches on `origin/main`, the plan is in exactly the state
   this step exists to prevent, and the run has not shipped its record. Report
   the failure rather than the stamp
   [policy: unknown-must-not-render-as-a-default].

   This is standing authority, not an escalation: `git-operations`
   `closeout-push` grants committing and pushing docs/plans diffs to
   `qontinui-dev-notes` or a `plans/` dir for a session-owned closeout, and its
   *"verify branch == origin after"* bound is what the read-back discharges.
   `merge-authority` is not in tension — coord is sole merge authority for
   `qontinui/*` **application** repos, and a notes/plans repo is neither. If the
   plans repo IS one coord lands, open a PR for the plan branch instead
   ([policy: pr-create-preference-order]) and let coord land it; the read-back
   above is still what closes the step.

   If the `rev-parse --is-inside-work-tree` check fails, the plan directory is a plain folder: the
   stamped file on disk **is** the record, there is nothing to commit, push or
   read back, and you must not create a repo to hold it.

   **FALLBACK ARM — when the direct land above is not available, a pushed branch
   is NOT the closeout: assert a PR carries the push (`coord-ff-lands.md`), re-checked before and after EACH push.** The
   land-on-`main` recipe is the primary route and `git-operations` `closeout-push`
   is its authority for a notes/plans repo. Where the plan repo IS one coord
   lands, that route is closed to you and a pull request is the only way the stamp
   reaches `main` — and a branch pushed with no PR never gets there: the stamped
   plan is published and permanently invisible to every `origin/main` reader.
   Measured 2026-09-02, **9** plan stems were pushed to `origin` and never
   proposed at all — no PR in any state, on any branch carrying the stem —
   including one this very closeout had pushed the day before. (A further 23 were
   on a pushed branch and on no `origin/main` while HAVING a PR: merge-train
   business while those PRs are OPEN.) **Once per chain is not enough.** coord
   can land the plan PR mid-chain and leave it CLOSED, MERGED or even OPEN, and
   every later push to that same branch is then stranded again. So before each
   push, and again after it, apply
   `knowledge-base/qontinui-specific/coord-ff-lands.md` → "Pushing to a branch
   whose PR may already have landed" to the branch you are about to push:
   ```bash
   BRANCH="<the branch you are about to push; after a fresh-branch cut, the NEW branch>"
   gh pr list --repo <owner/repo> --head "$BRANCH" --state all \
     --json number,state,headRefOid,url \
     --jq '.[] | "\(.number) \(.state) \(.headRefOid) \(.url)"'
   ```
   `--state all`, not `--state open`: an empty open-only answer cannot tell
   "never proposed" from "closed under you". A CLOSED or MERGED PR HAS been
   proposed, so it is not Step 0's pushed-but-unproposed class, but it carries
   nothing pushed after its close.

   When the section says no PR carries the push and commits remain unlanded,
   take its fresh-branch path. Open the new PR with **`coord_create_pr` first,
   then `gh pr create`**, carrying the line-anchored `Plan: <stem>` marker Step
   4.5 specifies. **Never `gh pr merge`, never `--admin`**: coord is the sole
   merge authority and that spelling is denied to agents fleet-wide. If the plan
   branch is the same branch as the implementation PR, that PR satisfies the
   assertion only while the same section says it carries the push; say so rather
   than opening a second one. The content read-back above also catches a
   stranded stamp push directly, whatever state the PR reads. Runbook:
   `knowledge-base/qontinui-specific/bodyless-work-units-and-stranded-plans.md`.

   **Do NOT POST a `shipped` work-unit transition:**
   unlike `in_progress` (Step 0.5), `shipped` is a DERIVED status — coord
   computes it from the work unit's landing predicate, so a direct
   `to_status:"shipped"` POST is rejected with `status_is_derived`. Coord flips
   the unit to `shipped` itself once the landing gate clears; your job is only
   to ensure that gate exists/clears (Step 6.5 — a `pr_merged` or `commit_live`
   gate anchored to the plan's work unit), not to set the status by hand.

4. **Clear coord activity status (per Step 0.6.5).** Fire the final
   clearing `POST /coord/status` documented in Step 0.6.5 with
   `current_task: null` so the dashboard tile stops showing this plan
   as in-flight. Failure is non-fatal — `prune_stale()` TTLs the row
   within an hour.

5. **Release the plan reserve (per Step 0.48) — only if THIS run acquired it.**
   Fire the `POST /claims/release` documented in Step 0.48 with
   `kind: "semantic_resource"`, `resource_key: "plan:<plan-stem>"` and the same
   `agent_session_id` used at acquire, in the same try/finally as the phase
   releases. **Skip it if Step 0.48 returned `renewed`** — an outer `/vet-imp`
   acquired the reserve and owns the release; dropping it here would unreserve
   the plan while that chain is still running.

6. **Cite the review arm — and read its ABSENCE as the failure signal.** The
   closeout report names, per PR opened, the arm Step 4.4 took, the disposition
   it quoted, and the artifact it read them out of
   (`~/.qontinui/review-arm/<session-id>.json`, resolved from the same
   `${QONTINUI_AGENT_SESSION_ID:-${CLAUDE_CODE_SESSION_ID:-}}`). **If the file is
   not there, that is the finding, not a formatting gap**: either this run opened
   PRs without ever raising the gate, or Step 4.4 resolved no session id and
   wrote nothing. Report which of the two you observed and name the path you
   looked at — an absent artifact reads exactly like a run that reviewed
   silently, and narrating past it is how occurrence 6 reached closeout with four
   PRs and no arm. An artifact whose `prs[]` does not cover every PR this run
   opened is reported the same way: name the uncovered PRs. **Cite Step 4.7's
   verdict beside the arm**, per PR: `corroborated`, or the contradiction /
   UNKNOWN row it left — a report that names an arm and no corroboration is
   reporting only the half the implementer wrote.

7. **Stop the heartbeat loop (per Step 0.48).** After the releases above, in the
   same try/finally, `remove` the plan row and then:

   ```bash
   bash <workspace-root>/qontinui-claude-config/scripts/coord-claim-heartbeat.sh stop --ledger "$CLAIM_LEDGER"
   ```

   `stop` is idempotent and safe to run **once the `remove`s above have emptied
   the ledger** — which is the whole point of doing them first, and `remove` of
   the last row already ends the loop by itself.

   ⚠️ **`stop` now REFUSES (exit 6) while the ledger still holds rows, and that
   refusal is information, not a failure to route around.** This ledger is keyed
   on `$CLAUDE_CODE_SESSION_ID`, which a subagent **inherits** from its parent, so
   one file is shared by every context under one harness session and the loop
   `stop` kills is the sole renewer of *all* of it. Rows left at this point mean
   either a `remove` above did not run, or a sibling context is still holding
   claims — report which, by name, and do **not** reach for `--force` to get past
   it. Skip the `remove`/`stop` pair on `renewed` exactly as the release is
   skipped — the outer `/vet-imp` owns that loop. **And do not read `renewed` as
   covering the subagent case:** a subagent working its *own* plan re-reserves a
   key its parent never took and gets `granted`, so this guard is silent in
   exactly the case it looks like it should catch (measured 2026-09-04). A loop
   left running past this point is a background process renewing claims nobody
   holds, until its own `--max-runtime` bound expires — which is strictly better
   than stopping one a peer still needs.

**Rule (c) applies to every write in this list.** Run
`bash <workspace-root>/qontinui-claude-config/scripts/coord-agent-refresh.sh` once before these closeout writes — the
work-unit transition, the gate attestations, any finding, and both claim
releases — and act on the verdict line. This is the point in the run where the
token is oldest, and a `REFUSED`/`EXPIRED` here silently drops the closeout
writes that make the work visible to every other session.

This step is mandatory. Plans without a status stamp lose context within weeks — and the stamp, not the file's location, is what tells a future agent whether work is still pending. A stamped plan sitting next to unstamped ones is the intended end state, not clutter.

#### Report the continuation work outcome — only when this run actually shipped

If this session was spawned as a **coord gate continuation**, the gate is still
carrying the runner's `spawned` — a value written the instant the terminal
appeared, which answers *"did a process start?"* and never *"did the work
happen?"*. This session is the only actor that can answer the second question,
and the SHIPPED stamp above is the moment it can honestly answer
**`work_completed`**.

**Be strict about what "completed" means here, or the value stops meaning
anything.** Post `work_completed` **only** when this run stamped SHIPPED at
item 1 with every phase landed. A run that stamped anything else, that
registered a Step 6.5 gate for a deferred or blocked phase, or that handed its
remaining work to a continuation of its own has **not** completed the work — it
deferred it, and `work_completed` would suppress the very detector
(`consumed_continuation_no_work`) that exists to notice. Post nothing here in
that case; `/blocked` owns the `work_abandoned` arm, and the runner's PTY-exit
fallback owns the silence.

**The precondition is two environment variables, and you read them BY NAME:**

```bash
GATE_ID="$(printenv QONTINUI_GATE_ID)"
GATE_DEVICE_ID="$(printenv QONTINUI_GATE_DEVICE_ID)"
```

Never an `env` dump — the session environment carries plaintext passwords and
the habitual `JWT|KEY|TOKEN|SECRET` redaction filter matches no variable named
`PASSWORD`. The runner injects both **only for a genuine gate continuation**:
coord's payload slot is overloaded, and a work-unit DAG dispatch reuses the same
frame with a `dispatch_id` and no `coord.gates` row at all. **Either variable
absent means there is no gate to report to — skip this silently**, never guess a
gate id, and never substitute a device id from elsewhere (the outcome UPDATE
carries `AND continuation_consumed_by = $3`, so a wrong device id writes nothing
and says nothing).

**The call** — coord's unauthenticated device-keyed data-plane ack, so no bearer
(`$COORD_HTTP_URL` defaults to `https://coord.qontinui.io`):

```bash
curl -sS -X POST \
  "$COORD_HTTP_URL/coord/gates/$GATE_ID/continuation-consumed" \
  -H 'Content-Type: application/json' \
  -d "{\"device_id\":\"$GATE_DEVICE_ID\",\"outcome\":\"work_completed\"}"
```

No `detail`: coord persists `work_completed` **bare**, deliberately, so the
transition guard can compare it exactly. Sending one is discarded.

**Read the response — the 200 is not the answer, `outcome_recorded` is.** coord
echoes what it actually persisted; anything other than a bare `work_completed`
back, a non-2xx, or a missing field is **a failure to report**, and you say so
in your closing report rather than counting the write. A producer that reads a
200 as success reproduces exactly the silent-success defect this route was fixed
to expose. coord permits one `spawned → work_*` transition and refuses
`work_* → work_*`, so a refusal means an outcome already stands — name the value
that stands instead of claiming yours landed.

**Best-effort, never blocking.** A missing variable, an unreachable coord or a
refused write never blocks the stamp, the commit, the release, or Step 6.5. It
is reported, not swallowed: one line naming the `gate_id` and the
`outcome_recorded` coord echoed, or the failure you actually saw.

### Step 6.5: Offer to register a coord gate for any deferred/blocked phase

*(canonical spec: `_gate-registration` — keep copies in sync)*

This fires whenever a phase is **deferred or blocked on an observable condition**
— a phase agent failed/aborted on something coord can watch (a PR must merge, a
deploy must go healthy, CI must go green, a metric must cross a threshold, a time
window must elapse, an operator must approve), OR Step 6 records a "follow-up
plan with open items" that waits on such a condition. A deferral with no
observable trigger (open-ended TODO) is NOT a gate — skip those.

**Prefer a work-unit dependency over a `unit_ready` gate for "phase N+1 waits on
phase N."** A deferral that is *purely* an in-graph dependency on another
work-unit reaching a terminal status is NOT a gate case: declare the edge to
coord (`POST /coord/work-units/:slug/deps {"depends_on":[<upstream-slug>...]}`, or
`metadata.depends_on` on upsert if it 503s pre-migration) and set the unit's
`metadata.dispatch` payload — coord's DAG scheduler auto-dispatches it when the
upstream reaches terminal status. Reserve gates below for *out-of-graph*
observable conditions (PR merge, deploy/CI, metric, time window, operator
approval). (canonical spec: `_gate-registration` → "DAG-dependency dispatch
supersedes unit_ready for the dependency-gated case".)

- **Default = explicit offer.** Ask via `AskUserQuestion` (header `Register
  gate?`, options Register / Skip), showing the derived anchor, predicate kind,
  and human-readable condition. Under opt-in auto mode (env `QONTINUI_AUTO_GATE=1`)
  register WITHOUT asking and report what was registered (gate_id + predicate).
- **Anchor (zero user input):** `work_unit_id` (a UUID) from
  `POST $COORD_HTTP_URL/coord/work-units/upsert` with the plan stem as `slug`
  (capture the returned `work_unit_id`; or the device-authed
  `GET /coord/agent-work-units/<slug>` — the operator `GET /coord/work-units/<slug>`
  403s a device JWT);
  `phase_name` from the phase heading. Anchor = (work_unit_id, phase_name). The
  `unit_ready`/`unit_status` predicates carry this UUID, not the slug. Claim-bound
  deferrals use the claim-anchored shape (`claim_kind`+`resource_key`) instead.
  With the UUID captured, also record it where the WIP-custody Stop hook reads
  it — this is the only step of this run that holds a real one, so without it the
  custody record's `work_unit_id` stays `null`:
  `bash qontinui-claude-config/scripts/custody-intent-write.sh <this worktree>
  work_unit_id=<uuid> plan_slug=<plan-stem>`. It merges rather than replaces, so
  it does not erase the `plan_slug` / `intent` `/preflight` Step 5 wrote, and it
  exits 0 on every path.
- **Register:** prefer MCP `coord_register_gate` (kinds: `pr_merged`,
  `deploy_healthy`, `claim_terminal`, `operator_approval`, `ci_green`,
  `ref_exists`, `metric_threshold`, `time_elapsed`, `unit_ready`,
  `migration_at_head`, `infra_drift_clear`, `file_exists`, `sql_count`,
  `unit_status`, `gate_cleared`, `commit_live`, `runner_served_sha`; plus — **exception cases only,
  see the Continuation bullet below** — an optional typed `continuation` or legacy
  `continuation_prompt` e.g. `run /implement-phase <stem> "Phase N"` for
  auto-resume). **HTTP fallback** when MCP is unavailable — for a plan-anchored gate
  it is now TWO device-authed calls on coord's `require_jwt` sub-router
  (device/agent/service JWT all work): (1)
  `POST $COORD_HTTP_URL/coord/work-units/upsert {slug, title?, status?}` →
  **capture `work_unit_id`**; (2)
  `POST $COORD_HTTP_URL/coord/work-units/<slug>/register-gate`
  `{predicate, phase_name (required), continuation_spawn?, clearance_audience?, gate_class?}` —
  `register-gate` does NOT upsert (404s `work_unit_not_found` if you skip step 1).
  Reach the two routes over a held device JWT, the `/coord-mcp` proxy (injects a
  device JWT), or the acting-user-service token (`coord-acting-bearer.sh`); MCP
  `coord_register_gate` now works from a device session too. A claim-anchored gate
  (no slug) uses MCP or `POST $COORD_HTTP_URL/coord/gates/register` (default
  `https://coord.qontinui.io`). Tenant derives server-side — never pass it.
- **Continuation = OFF by default.** Register the gate, but **omit the
  continuation ENTIRELY — no `continuation` and no `continuation_prompt` (MCP
  `coord_register_gate`), and no `continuation_spawn` (HTTP `register-gate`)**.
  All three spellings are the SAME knob: coord materializes both MCP fields into
  the DB's `continuation_spawn` column and both spawn, so passing
  `continuation_prompt` while faithfully "omitting `continuation_spawn`" still
  produces the duplicate run. The default is **omission** (`continuation_spawn`
  NULL) — *not* the typed `{"action":"notify_only"}`, which STORES a payload and
  is a different DB state (use that only for a deliberate typed no-op). Under
  charter rule 10 ("Finish to zero") this session finishes its own follow-ups, so
  a redundant continuation queues a duplicate, parallel run of the same work (the
  concurrent-WIP clobber the coordination layer exists to prevent). Attach one
  only when the follow-up will **outlive** this session: a wait longer than rule
  10's ≲2h monitor window; this session ending WITHOUT dispatching the follow-up
  itself; an `operator_approval` / human-decision gate (unbounded in time — but
  sensitive work stays notify-only unconditionally); or a cross-session chain
  owned by a different work unit or device (**out-of-graph only** — a purely
  in-graph dependency on another work unit is a DAG edge + `metadata.dispatch`,
  not a gate). **And never arm one on a gate whose predicate is already satisfied
  at registration** — a born-cleared gate dispatches on the next 10 s sweep tick,
  so its continuation is a net for nothing; coord drops it and warns
  `continuation_dropped_born_cleared:` (keep the gate — that warning is NOT the
  registered-but-not-usable signal). **"Born cleared" describes the PREDICATE at
  the door, never the row**: the row is born `open` and the first sweep tick
  clears it, so such a gate is not terminal and a later sibling still pins it
  back `Open` (canonical: `_gate-registration` → "Registration warnings" →
  *What "born cleared" means*). Put the dispatch on a separate gate whose
  predicate is genuinely unsatisfied. Sessions also die exogenously (usage limit,
  crash, reboot) — if
  you are *stopping* incomplete-because-WAITING, that is `/blocked`'s
  session-close protocol and it DOES take a continuation. **Clearance stays
  record-only:** a continuation-less gate produces no dispatch and no
  `coord.alerts` row on clear — the gate row + dashboard + `all_cleared` event
  are the clear-time signal. **Failure now alerts regardless of continuation:**
  a gate going `failed`/`misconfigured` raises a `gate_unclearable_terminal`
  alert (`misconfigured` pages critical immediately; `failed` pages warning
  after a 15-min grace), and a gate rotting open past ~7 days surfaces via the
  gate doctor / info-level non-paging alerts. And if you DO
  rely on a spawn, delivery is a live defect — continuations are being dispatched
  but never consumed, and coord's pending window (**7 days** since 2026-07-23,
  widened from 24h) drops them permanently — so
  treat it as best-effort and read the gate's `continuation_consumed_outcome`
  (a **null** outcome means never claimed, which is worse than a recorded
  `spawn_failed`). (Canonical: `_gate-registration` → "Continuation policy".)
  - *Which half of this section's trigger you are in matters.* The
    **phase-abort** half — a phase agent failed/aborted on an observable
    condition — **is exception 2**: this session is not going to dispatch the
    follow-up, so **continuation ON** there, exactly as `/implement-phase`'s
    blocked-exit path says. The OFF-by-default headline above governs the other
    half: a Step 6 "follow-up plan with open items" that this session is still
    carrying.
  - **When it IS on, populate `hint` — it is the spawned agent's only context.**
    coord flattens the continuation to `run /<skill> <args>` and appends
    `\n\n<hint>` only if a hint is present; no other key in the frame carries a
    plan body, a PR body, prior findings or the predicate, so a hintless
    continuation spawns an agent that knows nothing but the command line. Six
    labelled lines, **references before bodies**, **≤2000 characters**:
    `Plan: <plan-stem>` / `Why: <≤2 sentences>` / `PR: <owner/repo#N>` /
    `Blocked: <block_reason_code>[ ×N since <ISO8601>]` /
    `Resource-keys: <keys a peer's coord_recent_findings would match>` /
    `Tried: <what this session did, and what it deliberately did NOT do>`. Omit
    a line you have no value for; never write `unknown`. Nothing enforces the
    cap and nothing truncates for you — cut prose, keep references, and end a
    cut brief with
    `[hint truncated at 2000 chars -- full context: <plan-stem> / <owner/repo#N>]`,
    because a silent truncation is the failure mode. `continuation_prompt` has
    no hint field: append the brief to the prompt after a blank line. (Canonical:
    `_gate-registration` → "The brief — a continuation with no `hint` is a fresh
    agent with no context" — keep copies in sync.)
- **`clearance_audience`:** set `agent` for agent-verifiable facts ("/vet-plan
  was run", "crate exists + tests green", "a dual run emitted evidence") so the
  session that completes the work can attest the gate itself; set `operator` for
  business/judgment/strategy or on-page-human-verification gates. Default is
  `operator` if omitted; the sensitive-work rule always forces `operator`.
- **`gate_class`:** classify the gate so coord's per-tenant `gate_clearance`
  matrix can resolve who may clear it. `security-surface` when the deferred work
  this gate guards would itself fire a `security-and-autonomy` glob or content
  trigger (name the trigger in `phase_name` so it stays auditable — a
  CLAIM-anchored gate has no `phase_name`, so name it in the plan or report
  instead);
  `ops-confirm` for deploy/sweep/migration/config confirmations;
  `routine-review` for mechanical follow-ups. **Omit when none applies** —
  omitting is safe and never a loophole, and a guessed class is worse than none.
  ⚠️ **The old "`agent_non_author` means nobody may attest — this is a ONE-DEVICE
  fleet" warning is SUPERSEDED (re-verified 2026-08-30).** Both premises changed:
  the fleet has **four** device ids (`eb2155ed4152`, `c79a07d57e40`,
  `84c0229232cb`, `3e7e4b0475de`), and `non_author_allows_identities` is now a
  six-tier ladder in which **tier 3 (different device)** and **tier 5 (same
  device, differing VERIFIED sessions)** both resolve to NON-author. It refuses
  only in tier 6 — same device, no proven session on either side. So
  `agent_non_author` IS usable when the clearer is a different device or carries
  proven session identity. ⚠️ **The sentence that used to follow — "the work-unit
  attestation check now routes through this SAME ladder" — is FALSE and was
  removed 2026-09-03.** Verified on qontinui-coord `origin/main`:
  `work_unit_registry::authorize_target_transition` takes two `Option<&str>`
  keys and does a flat `owner == attester` compare;
  `non_author_allows_identities` is called only from `gates.rs`. The ladder is
  real for GATES and fictional for work-unit attestation — do not carry it
  across. For the work-unit rule read policy live rather than restating it:
  `/policy get policy plan-discipline` and `verification-and-evidence`
  [policy: never-pin-a-mutable-policy-value]. (Canonical for gates:
  `_gate-registration` → "`gate_class`".)
- **Predicate choice:** wait-on-PR (non-coord repo) → `pr_merged`; work landing
  on a **coord-orchestrated repo** → `commit_live` `{repo, commit_sha}` with a
  **post-land main SHA** (NEVER a pre-land branch-head SHA — rebase-land rewrites
  SHAs so the gate rots open, gate `c14d103c` 2026-07-11; or anchor `unit_status`
  instead — **NOT `file_exists`, which is broken, see below**); wait-on-deploy →
  `deploy_healthy`; wait-on-CI → `ci_green`; burn-in / wait-N-days →
  `time_elapsed`; metric condition → `metric_threshold` (explicit `labels` — e.g.
  `coord_ci_runner_count` MUST filter `{status:"idle"}`); a vetted plan that is
  ready, dispatchable work → `unit_ready` `{work_unit_id, ready_status}` —
  transition the unit FIRST and set `ready_status` to the status that actually
  landed (`vetted`, else the Free fallback `vetted_unattested`); a hardcoded
  Attested value on a unit you own never clears, since an owner may not attest
  (canonical: `_gate-registration`). (**NOT**
  `operator_approval` — `operator_approval` is for genuine human decisions, not a
  work queue); schema/alembic-at-head → `migration_at_head` `{schema}`; infra drift
  cleared → `infra_drift_clear`; a repo file/workflow existing → ⛔ `file_exists`
  `{repo, path, on_ref?}` — **usable again; the 2026-08-05 fleet-wide 403 was FIXED
  by coord `e6f486b8` (2026-08-15) and that fix is deployed** (ancestor of the
  serving build, re-probed live 2026-08-31: registered `201`, `verdict: cleared`).
  Residual: that probe was one PUBLIC repo, so re-probe before relying on it
  against a private one;
  a coord data count crossing a bound
  → `sql_count` `{query_id,op,n}` (whitelisted `query_id`, never raw SQL); an
  umbrella plan reaching a status → `unit_status` `{work_unit_id,status}`; another
  cross-anchor gate clearing → `gate_cleared` `{gate_id}`;
  needs-human → `operator_approval`. Anything **security / credential / billing /
  strategy-sensitive** registers as `operator_approval` + notify — never an
  auto-resuming gate, never silently auto-registered.
- **Masked-tool honesty:** per-agent MCP allow-set curation can mask
  `coord_register_gate` as unknown (coord `mcp/mod.rs`). If the call fails as
  unknown/method-not-found, report **"gate NOT registered — coord_register_gate
  not in this session's tool allow-set"** and fall back to HTTP (or surface to the
  operator). NEVER report a gate registered without a returned `gate_id`.
- **Warnings honesty — a `gate_id` is necessary, NOT sufficient**
  [policy: `coordination` `gate-warnings-mean-not-usable`]. **Branch on the
  VERDICT, never on `warnings[].is_empty()`.** The gate is
  **REGISTERED-BUT-NOT-USABLE** when `initial_verdict_reason` says the predicate
  **cannot be evaluated**, or when `initial_verdict` is a terminal state it can
  never clear from (`misconfigured` / `failed`) — the row was written and the
  gate can never clear. **A non-empty `warnings[]` is NOT that signal:** most
  warnings are informational — every `pr_merged` gate on a coord-orchestrated
  repo carries one, and `continuation_dropped_born_cleared:` drops only the
  continuation while leaving a healthy gate. Read the warning text; do not count
  warnings. When the verdict test DOES fire, do NOT report the deferred item gated:
  re-check with `coord_check_gate_predicate {predicate}` **against a control
  whose answer you already know** (identical output on the control proves the
  predicate is dead, not your anchor), re-register on a predicate coord can
  evaluate, withdraw the unusable one (`coord_withdraw_gate`), and quote the NEW
  `gate_id`. Canonical: `_gate-registration` → "Registration warnings".
- **Dead-transport honesty (the OTHER mask):** a call that returns **`"Command
  failed with no output"`** is a *dead cached transport*, not a masked tool — the
  tool is present and listed, so the fallback above never fires. Presume the
  registration **LOST** (8 of 8 prod-adjudicable "no output" writes were adjudicated
  lost on 2026-07-26, four of them `coord_register_gate`), run **`/coord-revive`**
  for a typed verdict naming the door that is live right now, re-issue there, then
  **verify by read** (`coord_gate_inspect(gate_id)`, or the anchor filter
  `GET .../coord/agent-gates?work_unit_id=<uuid>&phase_name=<name>`). A retry's success is
  never evidence the original landed. The same applies to `coord_attest_gate` below.
  Canonical: `_gate-registration` → "Dead-transport honesty".
- The optional plan-file `## Gates` block is a **local convenience mirror only** —
  coord is the source of truth; never require it, never read it back as
  authoritative.

**Attest-on-completion (close the loop).** When this run instead COMPLETES work
that a registered gate was watching (e.g. a deferred phase that an earlier
session gated now finishes), it MUST attest that gate — otherwise an agent-fact
gate rots open until a human clicks it.

- **Find the gate:** by the `gate_id` recorded at registration, or by lookup
  `GET $COORD_HTTP_URL/coord/agent-gates?work_unit_id=<id>&phase_name=<name>` — the OPEN
  gate whose condition the completed work satisfies. That is the **device-authed**
  read door; the operator `GET /coord/gates` is `TenantId`-only and 403s a device
  JWT (a wrong-door 403, not a missing gate).
- **Attest (unchanged — keyed by `gate_id`):** prefer MCP `coord_attest_gate` (pass
  `gate_id` — works from a device session since attest takes no upsert); fall back to
  the device loopback forwarder `POST http://127.0.0.1:{runner_port}/coord-mcp/gates/{gate_id}/attest`
  (header `X-Coord-Mcp-Proxy-Key`, or `Authorization: Bearer <nonce>` on configs
  written after the Phase 2 header move — no body bearer; maskless fallback), then the
  direct device-authed `POST $COORD_HTTP_URL/coord/gates/:gate_id/attest`. Tenant
  derives server-side — never pass it. Legal only on an OPEN `operator_approval`
  gate with `clearance_audience = 'agent'` in the caller's own tenant; coord flips
  it to `cleared` and fires the same fanout as operator approve.
- **Masked-tool honesty:** if `coord_attest_gate` is unknown/METHOD_NOT_FOUND it
  isn't in this session's allow-set → fall back to the HTTP attest route. NEVER
  claim a gate attested without a returned cleared `gate_id`.
  A **`"Command failed with no output"`** attest is the *dead-transport* failure, not
  allow-set masking — the tool was present, so that fallback never fires: presume the attest
  **LOST**, run **`/coord-revive`**, re-issue over the live door, and read the gate
  back to confirm `cleared`. A lost attest is the quiet one — the gate rots open
  while this run reports the item done. Canonical: `_gate-registration` →
  "Dead-transport honesty".
- **Honesty:** NEVER report a deferred item as done without EITHER a cleared
  `gate_id` OR an explicit "gate not found" note.

**Continuation cancel + mute (refresh + takeover).** A continuation-carrying gate
— since 2026-08-30 that is the `time_elapsed` **net** gate beside the `unit_ready`
record gate, not `unit_ready` itself — may have a queued runner-terminal spawn. If
you **re-register** a gate for the same plan/anchor, or this run **directly takes
over** the work that continuation was queued for (the active takeover wiring lives
in Step 0.6), retire it so the runner does not spawn a redundant terminal: find it
via `GET $COORD_HTTP_URL/coord/agent-gates?work_unit_id=<id>` (rows carrying a
`continuation_spawn` with `continuation_consumed_at == null ∧
continuation_cancelled_at == null` — **pre-dispatch rows included**), then
`coord_cancel_continuation` `{gate_id, reason}` — or its REST twin
`POST $COORD_HTTP_URL/coord/gates/:gate_id/agent/continuation-cancel` `{reason}`,
same capability, pick whichever transport is alive — **followed by** the mute,
which has the same two doors: `coord_mute_gate` `{gate_id}`, or its REST twin
`POST $COORD_HTTP_URL/coord/gates/:gate_id/agent/mute`
— the device-authed doors, so a device session does the WHOLE loop
itself: `/coord/agent-gates` discovers the row, `/agent/continuation-cancel`
retires the spawn (it is deliberately unguarded on `continuation_dispatched_at`,
so pre-dispatch is a supported stamp, not a 404), and `/agent/mute` stops the
dead net gate pinning the record gate `Open` as an open sibling. (The unprefixed
routes are the operator's and answer an agent 401.) Best-effort, never blocking:
404 = no such gate; **409 `already_consumed` = a spawn already happened, report
it honestly** rather than claiming the cancel landed — and mute regardless.
Narrate the retired `gate_id`. (canonical spec: `_gate-registration` →
"Continuation cancel + refresh".)

### Step 7: Run `/unattended` (mandatory, last action)

Once every other step is done — Steps 1–6 landed, and Step 6.5's
gate/attest handling is resolved for any deferred or blocked phase — invoke
the **`/unattended`** skill as the final action of this session, before
ending it. Not conditional on how the plan went: run it after a clean finish
exactly as after a partial one, and unconditionally on whether Step 6.5 found
anything to register — a plan with no deferred phase still gets Step 7.

`/unattended` answers one question this skill cannot answer about itself: if
no operator ever reads this session's transcript, does the work this session
just did still reach complete and correct? It reclassifies everything this
run produced (LANDED / WATCHED / RECORDED / IMPEDED / DROPPED) and converts
anything that exists only in the transcript — an unregistered gate, a
follow-up nobody will see, a deferred item this session assumed someone would
notice — into a durable store. That is a distinct check from Steps 1–6.5:
those steps land and gate the plan's own work; `/unattended` audits whether
this *session*, as a whole, survives going unread.

Do not fold this into Step 6 or skip it because Step 6 already stamped the
plan SHIPPED — the stamp records the plan's outcome, not the session's. Let
`/unattended` run to its own completion (it acts, then reports) rather than
treating it as optional closeout narration.

⚠️ **Reaching this step and invoking the Skill tool happen in the SAME
assistant turn, with the Skill call LAST.** This is the identical failure
class `/vet-imp`'s Step 3→4 handoff guards against — reproduced live there
2026-07-28: the agent wrote a closing summary and ended the turn without ever
calling the tool. Step 6's SHIPPED stamp is a natural-feeling stopping point
for exactly the same reason Step 3's VETTED confirmation was — it reads as
"the job is done". It is not: Step 7 is not a suggestion appended after the
report, it is the report's last line being a tool call, not a sentence. If
you have just finished Step 6 (or Step 6.5), the very next thing you emit is
the `Skill: unattended` call — never a summary, a "this plan is now
complete" sentence, or a hand-off line promising it runs next. **Never
narrate this step.** Do not write "next, running `/unattended`", "closing out
with the unattended audit", or any equivalent — narration is not action, and
writing that sentence is how this step gets skipped. If you find yourself
about to write it, call the tool instead; the tool call IS the sentence. This
applies whether `/implement-plan` was invoked directly or as `/vet-imp`
Step 4's nested call — the orchestrator's own Step 5 (final session name,
corroboration, report, reserve release) runs after this step returns, not
instead of it.

## Rules

- **Never narrate the close-out.** Do not write "now running `/unattended`", "closing out with the unattended audit", or any equivalent as the last text of a turn once Step 6 (and Step 6.5, if applicable) are done. Narration is not action: the SHIPPED stamp is a stop cue, exactly like `/vet-imp`'s VETTED stamp, and this is the same diagnosed failure (2026-07-28) applied to Step 7 instead of a hand-off — the agent writes the closing sentence and ends the turn without ever calling `Skill: unattended`. **Step 6/6.5 completing and the `Skill: unattended` call MUST occur in the SAME assistant turn, with the Skill call LAST.** If you find yourself about to write that sentence — call the tool instead. The tool call IS the sentence.
- **Phases run as Agents, not Skill calls** — this keeps implementation work out of the main context
- **Never stop between phases** — the entire plan executes in one session
- **Complete ALL work** — never skip tasks due to size or complexity
- **Fix, don't report** — fix issues immediately, don't just list them
- **Parallel by default** — launch independent phases concurrently; only serialize when there are true data dependencies
- **Edit work runs in an allocated worktree, never the primary checkout** — before launching a phase Agent that will `Edit` / `Write` / run `git` against a coord-registered repo, the coordinator allocates an isolated git worktree for that repo and passes that path as the Agent's working directory. The Agent treats that path as its repo root; every edit lands there, never in the operator's primary checkout. Sibling to the `/manual-test` "never touch the primary runner" rule — same shape (don't share the primary), different substrate (git worktree vs supervisor temp runner). Remove the worktree (and its isolated CARGO_TARGET_DIR, kept OUTSIDE the worktree) after the work ships. **Why:** see `plans/2026-05-28-isolate-session-edit-work-in-worktrees.md` — the proximate cause was two concurrent skill chains editing the same primary checkout simultaneously.

  **Allocate THROUGH coord, and declare the plan** *(plan `2026-08-16-plan-corpus-authority-and-run-provenance` Phase 1)*. Do not open with a bare `git worktree add`. Call coord first:

  ```
  POST $COORD_HTTP_URL/agents/allocate
  {
    "device_id":    "<this machine's device_id>",
    "repos":        [{"repo": "<repo>"}],          // parent_sha omitted → coord branches off clean origin/main
    "intent":       "<plan-stem>: <phase title>",
    "work_unit_id": "<the plan's work_unit_id UUID>",
    "build_required": true                          // for a Rust/src-tauri/Cargo.toml footprint
  }
  ```

  `work_unit_id` is the UUID from `POST /coord/work-units/upsert` (the same one Step 0.5 transitions) — **not** the slug. It is the first thing that has ever written `coord.agent_worktrees.work_unit_id`, and it is what lets cleanup reap by *plan* instead of by disk heuristics. A `work_unit_id` coord cannot resolve is dropped with a warning, never an allocation failure, so a stale hint degrades to today's behaviour rather than blocking the phase.

  **Why through coord and not by hand:** an agent *cannot* register a worktree it already created — `AllocateRepoSpec` is `{repo, parent_sha?}` only, the path is computed server-side by `suggest_worktree_path` and the branch by `decide_branch`, and `POST /agents/allocate-local` was removed as dead code in runner #443. There is no adopt-an-existing-path door. So a hand-rolled worktree is **undeclared**, and an undeclared worktree can be neither attributed to a session, nor pinned (`POST /coord/worktrees/:id/retention` is keyed on a ledger ROW id, which it does not have), nor drained by policy. It accumulates instead.

  **What this used to say, and why it changed** *(plan `2026-08-18-undeclared-worktree-exposure-and-classification`, Phase 2, 2026-08-19)*: this paragraph warned that undeclared worktrees were eligible for **automated deletion** at up to 25 removals per device per tick. That was true and it was the hazard — a clean, commit-less worktree passed every gate, because the one protection designed for it (the retention pin) was reachable only through the ledger row it lacks. `TriggerSignal::Undeclared` has since been withdrawn from the `remove` allowlist in `worktree_reclaim.rs::is_remove_trigger`, so an unattributable worktree is no longer destructively reapable at all. **The reason to allocate through coord is now a leak, not a loss** — a real cost, and the honest one. The env flags remain set and deliberately inert.

  **Two things the response can say that are NOT "here is your path" — handle both:**

  1. **`worktrees[].worktree_path` is RELATIVE**, e.g. `agent-worktrees/<agent_id>/<repo>`. This is deliberate: coord runs on Linux/ECS and a host-absolute path would be meaningless to a Windows runner, so the consumer re-roots it. Re-root it under the workspace root — `<workspace-root>/<worktree_path>` — and create the checkout there with the **branch coord reserved**: `git -C <repo> worktree add -b <worktrees[].branch> <absolute-path> <worktrees[].parent_sha>`. Use coord's branch verbatim; it was arbitrated against the `BranchName` claim so two agents declaring the same intent get distinct names.
  2. **`isolation.mode` may be `shared_branch` or `wait`, not `worktree`.** That field is coord's disk/build-slot budget decision (`policies::isolation`). On `wait`, do not force a worktree — report the `reason` / `blocking_resource` (typically `build_slot` or `disk_or_slot`) and retry, or fall back to a single serialized phase. On `shared_branch`, coord is telling you the canonical checkout can safely carry this branch. Treating every response as `worktree` re-creates the disk pressure the budget exists to prevent.

  **Sibling build-dependency worktrees count.** The `qontinui-schemas` checkout a Rust phase needs to satisfy Cargo path deps must be allocated the same way, with the same `work_unit_id`. It is the nastiest class precisely because it belongs to *none* of the plan's declared repos — so even a "worktrees for this plan's repos" query misses it, and only the work-unit link reaches it.

  **Known residual, stated so nobody reads silence as coverage:** this rule governs the worktrees *this skill* creates. `/manual-test`, `/manual-test-loop` and `/update-spec` were migrated to `POST /agents/allocate` alongside it (plan `2026-08-18-undeclared-worktree-exposure-and-classification`, Phase 3); what remains uncovered is a **human at a shell** running `git worktree add`, which no skill-layer rule can reach.

  **Correction, 2026-08-19:** this paragraph used to name the harness's own `WorktreeCreate` hook (a `worktree-create.sh` in the hooks directory, deleted 2026-08-19) as a generator of undeclared worktrees, "which branches `agent/<short-id>` in every sub-repo for every isolated subagent". **That was false and the hook is now deleted.** It was never registered in any settings file, so it never fired once: `.claude/settings.json` declares exactly four hook events (`SessionStart`, `Stop`, `PreToolUse`, `PostToolUse`), and the `.claude/worktrees/` directory it would have populated contained only hand-made single-repo checkouts, none matching its `agent-<short-id>/<repo>` shape. The claim originated here, was read as fact by a later session, and was shipped onward in a plan and a PR body before anyone checked — which is why the hook was deleted rather than left unregistered. **The Agent tool runs without worktree isolation, at the workspace root**; see `knowledge-base/qontinui-specific/guard-hooks.md`.

## Implementation Notes

$ARGUMENTS
