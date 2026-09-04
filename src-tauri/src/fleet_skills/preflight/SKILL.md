---
name: preflight
description: "Reserve-before-you-code protocol — run at the START of any plan or task, before writing the first line. Reserves the plan in coord, checks the durable plan registry for already-shipped work, flags a shared checkout parked on a branch whose PR already concluded, derives concrete globs, runs conflict_check + the merged-branch out-of-band search, declares intent with the plan slug, and acquires + heartbeats file-glob claims. If any overlap signal is non-empty, coordinate instead of proceeding. Prevents two agents from silently duplicating the same plan (the #564/#565-vs-#567 incident) — and, via its merged-branch search alone, from duplicating a peer's already-landed fix to a no-plan-file dispatch like a post-merge follow-up."
user-invocable: true
---

# preflight

**Mandatory "reserve before you code" protocol.** Run this checklist at the
START of any plan/task — *before* writing the first line of code. Its purpose is
to make duplicate plan work impossible to start silently: by the time you write
code, you have either (a) reserved the plan and confirmed no peer / merged work
overlaps, or (b) found an overlap and stopped to coordinate.

Every step below is verified-correct against live coord source — the tool names,
endpoints, and result fields are real. Do them in order; do not skip a step
because it "looks like a one-liner."

## Why this exists

The `#564/#565`-vs-`#567` incident: a completed, tested, pushed PR turned out to
duplicate already-merged work. coord — whose entire purpose is to let N agents
work a shared repo without colliding — surfaced **no** overlap signal at any
point (intent declaration, claim acquisition, or pre-push). The two agents never
saw each other, because:

- nobody reserved the plan key, so the live-concurrency mutex never fired;
- `conflict_check` (the one fleet-wide path check) was never run — and even if it
  had been, it scans only *unmerged* branches, so #564/#565 had already merged +
  deleted and dropped out of the signal;
- `declare_intent` derived **empty** globs server-side and warned about nothing,
  so the declared scope was invisible to every path-overlap check;
- claims were opaque `kind:resource_key` strings (not globset paths), 90s TTL,
  never heartbeated → unmatchable and expired.

This skill closes each of those gaps with steps the agent runs itself, no coord
server change required.

## Environment

- `COORD_HTTP_URL` — defaults to `https://coord.qontinui.io`. HTTP fallbacks
  below assume this base.
- **Plan slug** — the canonical cross-agent key. It is the plan filename **stem**
  (drop the `.md` and any leading date is part of the stem as stored, but match
  what `coord.plans` / `coord.sessions.plan_slug` / `plan_ready` gates use). For
  `plans/2026-06-13-coord-parallel-duplication-prevention.md` the slug is
  `2026-06-13-coord-parallel-duplication-prevention`. This single key is shared
  by `coord.plans`, `coord.sessions.plan_slug`, and `plan_ready` gates — one key
  space, not two.

## The checklist

### 0. Reserve the plan (free today — do this FIRST)

Call the coord reserve primitive, keyed on the plan **slug**:

```
coord_reserve_resource(kind="plan", name="<plan-slug>")
```

HTTP fallback (when the coord MCP tool is unreachable — and note this door is
**unauthenticated**, verified `422`-on-empty-body 2026-08-25, which is exactly
why it survives a dead MCP transport):

```bash
# Resolve identity ONCE, before the call. device_id is the canonical local key;
# machine_id is the legacy spelling. The WIRE field on /claims/* is machine_id
# regardless of which local key supplied the UUID.
MACHINE_ID="${QONTINUI_MACHINE_ID:-$(python3 -c 'import json;d=json.load(open("'"$HOME"'/.qontinui/machine.json"));print(d.get("device_id") or d.get("machine_id",""))')}"
AGENT_SESSION_ID="${QONTINUI_AGENT_SESSION_ID:-${CLAUDE_CODE_SESSION_ID:-}}"

curl -s --max-time 120 -X POST "$COORD_HTTP_URL/claims/acquire" \
  -H "Content-Type: application/json" \
  -d "{\"kind\":\"semantic_resource\",\"resource_key\":\"plan:<plan-slug>\",\
       \"machine_id\":\"$MACHINE_ID\",\"agent_session_id\":\"$AGENT_SESSION_ID\"}"
```

> ⚠️ **The `machine_id` + `agent_session_id` pair is load-bearing — do not drop
> it back out of this call.** Together they form the session-scoped **owner
> token** `<machine_id>:<agent_session_id>`. Without it the claim is *unowned*,
> and a SECOND session on the SAME machine silently **renews** the first
> session's reservation instead of seeing `held` — which is this checklist
> failing open on the single commonest collision shape in the fleet (both
> documented incidents were same-machine). That is precisely the bug plan
> `2026-06-03-coord-session-scoped-claim-owner-plan.md` fixed for `phase` claims
> (SHIPPED 2026-06-03, coord PR #271: `acquire` SETs/compares the token,
> heartbeat/release match on it, `Held`/`Stolen`/`ClaimHolder` carry the holder
> session). This fallback shipped without it, one key space over.
>
> Omit `agent_session_id` **only** if it resolves empty (older Claude Code) —
> send no key rather than an empty string; coord's `None` fallback then
> preserves the old machine-only behaviour. Any later
> `/claims/heartbeat` or `/claims/release` MUST replay the SAME
> `agent_session_id`, or it will not match and returns `not_held`.
> `scripts/coord-claim-heartbeat.sh` (step 6) replays the pair for you on every
> tick — **and a HAND heartbeat must too.** A hand-rolled `curl` that drops the
> owner token does not fail loudly: it answers `not_held` while the claim quietly
> ages out, which is the same unprotected-work outcome as never heartbeating at
> all.

**The `--max-time 120` is a floor, not a target — and it is load-bearing.** This
reserve used to pay a fleet-wide collision scan on top of the SET-NX, and that
scan was *volatile*: the same call, on the same code, measured **43.8 s cold on
2026-08-26** (47.3 s also observed) and **7.75 s cold on 2026-08-30**, warm
readings spread 2.4-6.0 s inside a single minute. A budget under the cold cost
does not report "slow" — it reports a **timeout**, which the lifecycle commands'
fail-closed arm correctly reads as an unreachable coord, so **a short timeout
turns a healthy coord into an abort** (two runs at 20 s failed exactly that way).
Since plan
`2026-08-26-mandatory-plan-reserve-cold-cost-trips-its-own-fail-closed-arm`
Phase 2 moved the scan off the synchronous reserve path, this call is expected to
answer at plain-acquire speed — a scan-free `phase` acquire on the same door
measured **0.33 s**. If it takes tens of seconds again, the cost has regressed:
report the number, do not raise the floor.

**The MCP arm's timeout is NOT settable from here.** `coord_reserve_resource`
runs on the MCP client's budget, so a failure of it that arrives *faster than the
`--max-time` floor above* is a suspected **client-side budget**, not evidence
that coord is down. Re-issue over the `/claims/acquire` fallback **with the
explicit `--max-time`** BEFORE concluding coord is unreachable — the timed
fallback is the cheap disambiguator between *slow* and *gone*. Only when the
explicitly-timed fallback also fails is the answer UNKNOWN. `/vet-plan` 0.2,
`/vet-imp` 1.1 and `/implement-plan` 0.48 state the same contract and the
fail-closed verdict it feeds.

The reserve mints a `semantic_resource:plan:<slug>` claim with an atomic SET-NX
and returns exactly one of:

- **`granted`** — you are the first agent on this plan. Proceed (run the rest of
  the checklist anyway — the reserve is the live layer, not the whole guard).
- **`fork_risk`** — **no longer returned; the reserve answers exclusion only.**
  The fleet-wide collision scan that produced this outcome moved off the
  synchronous reserve path (plan
  `2026-08-26-mandatory-plan-reserve-cold-cost-trips-its-own-fail-closed-arm`
  Phase 2 — it was 87-96 % of the reserve's cost, and it was already best-effort,
  degraded to an empty sibling list on any error), and `fork_risk` was a
  *discriminator computed from* that sibling list, so it cannot fire. Do not wait
  for it. A caller that wants fork risk asks for it explicitly:
  **`coord_predict_resource_collisions`** — the same predictor reserve used to
  call, reached directly. Its named siblings are overlap candidates; inspect them
  in step 4, exactly as this bullet used to say. If an older coord build still
  answers `fork_risk`, read it the same way — advisory overlay, not a hard
  holder — and proceed.
- **`held`** — a peer is **already implementing this plan** (the response carries
  the holder identity). Do **not** start. Coordinate (hand off / sequence behind
  them) or stop.

> Verified 2026-08-25: `coord_reserve_resource` is registered at
> `qontinui-coord/crates/coord/src/mcp/tools.rs:13575` (`reserve_resource_tool`),
> handler at `:13634`, and returned granted / held / fork_risk — the
> `fork_risk` arm is gone as of the Phase 2 change above, leaving granted /
> held. It maps
> `(kind, name)` onto a `ClaimKind::SemanticResource` claim keyed
> `format!("{class}:{name}")` (`:13669`). (The former citation
> `qontinui-coord/crates/coord/src/mcp/tools.rs:3183` was wrong on both the path — the crate
> lives under `crates/coord/` — and the line.)

### 0b. Is this checkout a sane place to start? (advisory — never gating)

Steps 1-7 all ask whether the *plan* is free. This step asks the different
question the reserve cannot: whether the *tree you are standing in* is somewhere
sane to begin from. A shared checkout can be byte-current with `origin/main` —
the axis `scripts/refresh-served-config.sh` and the SessionStart staleness
advisory already cover end to end — and still be parked on a feature branch
whose PR merged a week ago. Nothing else in this fleet reports that, and the
reserve in step 0 will happily grant a plan onto a dead branch.

> ⚠️ **This is the one step in this checklist whose door is not yet live.** It
> ships in the sibling `qontinui-coord` PR of plan
> `2026-08-28-shared-checkout-branch-provenance-and-reclaim-signal`; until that
> lands, the read 404s. That is a fail-open skip (see the last paragraph), not a
> reason to hold the checklist — but do not read the opening
> "verified-correct against live coord source" as covering this step yet.

**Run it only in a shared / primary checkout.** Inside a coord-allocated
worktree the branch already has provenance, written at allocate time by
`POST /agents/allocate`; this step would add nothing there and must stay out of
the way. The mechanical test is that a linked worktree's own git dir is not the
repo's common one:

```bash
# Skip 0b entirely unless this is the repo's canonical checkout.
if [ "$(git rev-parse --absolute-git-dir)" \
   = "$(git rev-parse --path-format=absolute --git-common-dir)" ]; then
  REPO=$(basename "$(dirname "$(git rev-parse --path-format=absolute --git-common-dir)")")
  BRANCH=$(git rev-parse --abbrev-ref HEAD)
  DEFAULT=$(git symbolic-ref --short refs/remotes/origin/HEAD 2>/dev/null | sed 's|^origin/||')
  [ -n "$DEFAULT" ] && [ "$BRANCH" != "$DEFAULT" ] && echo "0b applies: $REPO $BRANCH"
fi
```

`origin/HEAD` is written once at clone time and `git fetch` never refreshes it,
so a repo whose default branch was renamed still reads the old name here. That
errs toward calling a branch non-default, which costs one extra read on a step
that is fail-open anyway — it is not a reason to skip the read.

Then ask coord what that branch's PR actually did. Native tool when it is
visible:

```
coord_primary_tree_branch_status(repo="<repo>", branch="<branch>")
```

HTTP twin — **device-authed**, unlike step 0's door. Resolve the device JWT
through the cascade the `/gate` command's device-JWT residual documents
(`scripts/lib/coord-credential.psm1` is the shipped implementation, and it
reports a headless runner as a dead transport rather than a missing credential),
and stage it OFF argv with the same `printf`-into-a-header-file idiom every
other authed door in this corpus uses. `COORD_HTTP_URL` is the base from
**Environment** above:

```bash
COORD_HTTP_URL="${COORD_HTTP_URL:-https://coord.qontinui.io}"
AUTH=$(mktemp) || exit 0
trap 'rm -f "$AUTH"' EXIT
printf 'Authorization: Bearer %s\n' "$DEVICE_JWT" > "$AUTH"
AUTHP=$AUTH; command -v cygpath >/dev/null 2>&1 && AUTHP=$(cygpath -w "$AUTH")
curl -fsS --connect-timeout 5 -m 15 -H @"$AUTHP" \
  "$COORD_HTTP_URL/coord/agent-trees/branch-events?repo=$REPO&branch=$BRANCH"
```

> The `agent-` prefix is load-bearing. The unprefixed `GET /coord/trees/...` is
> the operator dashboard's `TenantId`-tier route and answers a device JWT
> `403 tenant_not_resolved` — the same operator/agent door split the gate verbs
> have, on a different surface.

The response has **three** states, not two:

| Response | Meaning | What to do |
|---|---|---|
| a row whose `terminal_outcome` is `pr_merged` or `pr_closed` | this branch's PR **concluded** — both values mean that same one thing here | surface the advisory below |
| a row whose `terminal_outcome` is null | the PR is still open, or none was ever opened | nothing to say |
| **no row at all** | **UNKNOWN** — nobody observed a checkout of this branch | **say nothing** |

**No row is UNKNOWN, and UNKNOWN is silent.** The events come from
`pre-checkout-coord-guard.sh` (qontinui-stack) observing a `git checkout -b` /
`git switch -c` — **not** from `git-guard.sh`, which has carried no
`checkout`/`switch` arm since qontinui-claude-config #567 narrowed it to coord's
merge authority. So a machine whose harness
installs no PreToolUse hooks (pi, Codex) and a branch a human created in a
terminal outside any Claude Code tool each produce nothing at all. Do **not**
emit "no concluded PR found", or any other reassuring line, on an empty read:
that converts "nothing was looking" into "I looked and it was clean" — the same
false-all-clear this checklist already refuses at step 4b's exit `3`, and the
`silent-empty-is-unknown` discipline served policy `verification-and-evidence`
states for every other cache in this fleet.

When `terminal_outcome` IS set, surface exactly one line:

> this checkout is parked on a branch whose PR already concluded
> (`<outcome>`, PR #`<n>`) — consider switching to `<default>` before starting

**Then proceed.** Advisory here means advisory: this step never switches a
branch, never deletes one, never stashes, and runs no other command against the
tree — and it does not offer to. Every piece of this fleet's tooling that
touches a shared checkout draws that same line, because the tree may be a peer's
live WIP and the judgement is the reader's, not this checklist's:
`scripts/refresh-served-config.sh` refuses rather than forces, and the coord
worktree-claim guard warns and allows.

It is also **non-gating and fail-open on every failure**: no credential, a
`403`, a coord that is unreachable, an unparseable body, or a coord build that
does not serve this route yet all mean the same thing — skip the step and start
work. The `--connect-timeout 5 -m 15` above is what keeps a black-holed host
from stalling the checklist. Nothing this step reads may delay or block step 1.

### 1. Durable cross-time check (catches already-merged work)

The reserve claim in step 0 is **TTL'd** — it expires and is released on
completion, so it does **not** survive a peer's merge+delete. It catches
*concurrent* agents, not *finished* work. For the cross-time signal, read the
durable plan registry:

```bash
curl -s "$COORD_HTTP_URL/coord/plans/<plan-slug>"
```

If `status` is `shipped` (or otherwise terminal) the plan is **already done** —
STOP. This is the durable signal the TTL'd claim and the path-based tools miss.

### 2. Derive concrete globs from the plan (not free text)

Read the plan's cited files / prior-art table and turn them into a **globset path
list**, e.g.:

```
qontinui-runner/src/components/terminal/*
qontinui-coord/crates/coord/src/mcp/tools.rs
```

Derive these from the plan content, not from a free-text summary. These exact
paths feed steps 3, 5, and 6 — server-side glob derivation is unreliable (it
returned `[]` in the incident), so you supply them explicitly everywhere.

### 3. `coord_conflict_check` with those globset paths

```
coord_conflict_check(paths=["<glob>", "<glob>", ...])
```

Inspect every signal in the response:

- `conflicting_branches` — unmerged branches touching the paths.
- `claim_holders` — live claims on the paths.
- `overlapping_peers` — peers with overlapping declared intent.
- `staleness_warning` — if present, the branch index is stale (>10m old); treat
  the response as **advisory** and lean harder on step 4 — do **not** trust a
  stale `clear`.

> Verified: `conflict_check` looks up claims as `ClaimKind::FileGlob` keyed on
> each requested path — a claim of a different kind/shape never matches, which is
> why steps 5/6 use globset path form.

### 4. Cover the merged-branch blind spot out-of-band

`conflict_check` scans only *unmerged* branches (`pr_state IN ('open','draft')`)
— the moment a peer merges and deletes its branch, the signal vanishes. Cover it
yourself:

```bash
gh pr list --state all --search "<plan-keywords>"
git log --all --grep "<plan-path>" --since=14d -- <changed-dir> [<changed-dir> ...]
```

A recent PR or commit citing this plan path → the work is **done**; stop. (In the
incident, the merged commits cited `Plan: plans/...` — exactly what this search
catches.)

**No plan path to grep for (e.g. a "post-merge follow-up" dispatch — see the
section below)?** Use the range-diff form instead, against the commit your
worktree/dispatch names as the starting point:

```bash
git fetch origin main
git log <branch-point>..origin/main --oneline
gh pr list --state all --search "<the target PR's own title keywords>"
```

A commit or PR in that range already addressing the diagnosed gap → the work is
**done**; stop. This is the SAME check as the plan-path grep above, adapted for a
dispatch with no plan file to cite — not a different, weaker substitute.

### 4b. Cover the uncommitted-WIP blind spot on disk

Step 4 covers work that is already **merged**. This step covers the other end of
the same timeline: work a peer has not published. Between them sits
`conflict_check`, which scans unmerged branches carrying an open/draft PR — so a
branch with commits but no PR, and a checkout with no commits at all, both fall
through every net in steps 0–4. The scanner covers both: uncommitted changes
(staged, unstaged, untracked, and unmerged/conflicted), and commits that exist
but have not been pushed.

```bash
bash <workspace-root>/qontinui-claude-config/scripts/scan-worktree-wip.sh \
     "<glob>" "<glob>" ...        # the same globs from step 2
```

The exit status is **three-state**, and the third state is the one that matters:

| Exit | Meaning | What to do |
|---|---|---|
| `0` | Looked everywhere asked; found no intersection. | Proceed. |
| `1` | A peer holds work intersecting your paths. The report names the checkout, its branch, and the intersecting files — tagged `WIP` for uncommitted changes or `UNPUSHED` for committed-but-unpushed commits. | **Stop and coordinate** (step 7). |
| `3` | **INCOMPLETE** — something could not be looked at (a `git` call failed, output was unparsable, the `--max` cap truncated the walk, or nothing was examined at all). | **Not an all-clear.** Fix the cause or widen `--max`, then re-run. Treating a `3` as a `0` reproduces the exact defect this step exists to prevent. |
| `2` | Usage error (bad option, no workspace root, or a glob the bash `case` dialect would silently under-match: `{a,b}` brace alternation, or a `[id]` bracket expression — which `case` reads as a character class, so a Next.js route glob would match nothing). | Fix the invocation and re-run. |

Only exit `0` clears this step. An INCOMPLETE scan tells you the answer is
UNKNOWN, not that the paths are free — and a false all-clear here is worse than
no scan at all, because it carries the authority of a check that was run.

Pass the globs — the unfiltered census walks every checkout in the workspace and
is far slower, and a census's exit status answers "does anyone have work?", not
"is *my* path free". The walk is bounded by `--max <n>` (default in the script's
`--help`); when the cap bites, the scan says so and exits `3` rather than
reporting a truncated walk as clean.

**What it cannot see**, since none of these registers as INCOMPLETE: a checkout
outside the workspace root (another drive, another machine), `.gitignore`d
files, and a peer's unsaved editor buffer. `--help` carries the same list.

**Why this step exists.** Every registry read in steps 0–3 answers *"has a peer
REGISTERED an interest?"*, not *"is anyone working here?"*. On 2026-08-19
`who_is_working_on` returned `verdict: "clear"` **twice** for paths a peer held
as ~30 staged files in a hand-made worktree with no claim, no intent row and no
PR; `coord_session_worktrees` returned zero rows for the same paths, because
nothing registers a hand-made worktree. A plan was authored and fully vetted
against a premise that peer had already implemented, and better. The information
was never missing — nothing was looking.

The scanner covers main checkouts as well as worktrees. That is not incidental:
in the same session the *other* live blocker was uncommitted edits sitting in a
shared main checkout on an unpushed branch.

**It reports, it does not judge.** File mtimes are printed as observations, never
as liveness claims — a reader's own `git status` rewrites `.git/index`, so an
abandoned tree can look seconds old. If the scan flags a peer, establish
liveness some other way (ask on the session bus, check for a PR) before deciding
whether to wait or take over.

### 5. `declare_intent` with the plan SLUG as `work_unit_id`

```
coord_declare_intent(
    work_unit_id="<plan-slug>",
    intent_globs=["<glob>", "<glob>", ...],   # the explicit list from step 2
)
```

Also set `plan_slug` on the session.

Then record the same context where the WIP-custody Stop hook can read it, so
this worktree's custody record names the plan it belongs to instead of the
`null` it has carried for 100% of worktrees:

```
bash qontinui-claude-config/scripts/custody-intent-write.sh <this worktree> \
    plan_slug=<plan-slug> \
    intent=<the intent_text, when one was given>
```

- **Deliberately no `work_unit_id=` here.** This step performs no upsert and no
  UUID comes back — the `work_unit_id` argument above IS the slug — so passing
  one from this call site could only ever repeat the slug. The real UUID is
  written by the step that mints it (`/vet-plan` §5.4, `/implement-plan`
  Step 0.48); the writer merges, so that later call adds the UUID without
  erasing the slug this one wrote.
- It exits 0 on every path and prints to stderr when it could not write. A
  session that cannot record its plan context is not blocked by that.

- Use the plan **slug** as `work_unit_id` — the same canonical cross-agent dedup
  key `coord.plans` / `sessions.plan_slug` use.
- Pass **explicit** `intent_globs` — never rely on server-side derivation alone.
  An **empty** `intent_globs` in the response is an **error → re-declare with
  paths**, not a no-op. (Empty globs are why the incident's intent was invisible.)

### 6. Acquire `file_glob` claims in globset path form — and heartbeat them

```
coord_claim_acquire(kind="file_glob", resource_key="<glob>")   # one per path
```

- Use **globset path form** (`qontinui-runner/src-tauri/src/session/*.rs`),
  matching what `conflict_check` scans — **not** opaque `kind:resource_key`
  strings or `{a,b,c}` brace lists (those are stored opaquely and never match a
  path-based collision query).
- **Heartbeat** the claims for the work's lifetime — with the helper, not by
  hand. The incident's claims were 90s TTL and never heartbeated → they expired
  and left the work unprotected, which is the failure this step exists to stop.
  `scripts/coord-claim-heartbeat.sh` runs the loop:

  ```bash
  CLAIM_LEDGER="$HOME/.qontinui/claim-ledger/${AGENT_SESSION_ID:-nosession}.ledger"

  # One `add` per glob. --ttl 900 is deliberate: the file_glob kind defaults to
  # 90s, and coord re-arms a heartbeat to the REQUEST's ttl_seconds — so a 90s
  # row would force a 30s cadence PER GLOB for the whole run.
  bash <workspace-root>/qontinui-claude-config/scripts/coord-claim-heartbeat.sh add \
    --ledger "$CLAIM_LEDGER" --kind file_glob --key "<glob>" --ttl 900

  bash <workspace-root>/qontinui-claude-config/scripts/coord-claim-heartbeat.sh start --ledger "$CLAIM_LEDGER"
  ```

  `start` detaches a background loop that re-heartbeats every row at min(row
  TTL)/3 with a 60 s floor. The trade `--ttl 900` buys: a helper that dies leaves
  a `file_glob` claim lingering up to 15 minutes instead of 90 seconds — bounded
  by `stop` in your try/finally and by the helper's own `--max-runtime`.

- **`status` is the contract — read it, do not assume the loop lives.**

  ```bash
  bash <workspace-root>/qontinui-claude-config/scripts/coord-claim-heartbeat.sh status --ledger "$CLAIM_LEDGER"
  ```

  One line per row plus a verdict on the **exit code**: `LIVE` (0) — pid alive
  and every row heartbeated within half its TTL; `STALE` (3) — pid alive but a
  row has gone quiet; `DEAD` (4) — no loop at all; `STOLEN` (5) — a row's last
  answer was `stolen`, and the loop exited rather than re-arm a claim now held by
  someone else. **Only `LIVE` means the claims are held.** The other three mean
  the claim is **UNKNOWN**, which is a re-acquire and a line in the report, never
  a shrug — a dead loop and a healthy one look identical to anything that never
  asks.

- **Release symmetrically**: `remove` each glob row, then `stop` the loop, then
  release the claims. A loop still running past the release renews keys nobody
  holds.

### 7. If any signal is non-empty → coordinate, don't proceed

`held` in step 0, a non-empty `conflicting_branches` / `claim_holders` /
`overlapping_peers` in step 3, a matching PR/commit in step 4, or **exit 1** from
the WIP scan in step 4b all mean the same thing: **stop and coordinate.** Request
a handoff, yield, sequence behind the peer, or pick a different work unit.

An **exit 3** (INCOMPLETE) from step 4b is not that signal and is also not an
all-clear — it says the sweep has a hole in it. Close the hole and re-run; do not
convert "I could not look" into "I looked and it was clean."

Step 0b's branch advisory is not on this list either, in the other direction: it
is a fact about the checkout you are standing in, not a signal about the plan,
and it never withholds the first line. Surface it and carry on.

Only an all-clear sweep — every gating step run, and step 4b at exit 0 — lets you
write the first line.

## Layer B conventions (apply these alongside the checklist)

- **Repo-default correlation topic.** All agents on repo `R` use the
  deterministic topic `repo:<R>` (e.g. `repo:qontinui-runner`) so `coord_orient`
  sees every peer on the repo without both sides guessing the same ad-hoc string.
  Reserve ad-hoc sub-topics for genuinely-scoped sub-efforts, layered on top.
- **Always pass explicit globs** to `declare_intent`. An empty `intent_globs`
  return is a retry trigger, not a no-op (step 5).
- **Claim keys in globset path form**, consistent with what `conflict_check`
  scans (step 6).
- **Scan disk, not just the registry.** A clear `who_is_working_on` /
  `conflict_check` means no peer REGISTERED — never that no peer is working
  (step 4b).
- **Cite the plan path in every commit body and PR body** (e.g.
  `Plan: plans/<plan-slug>.md`) so the work is reliably indexable by the
  merged-branch search in step 4 and the durable plan→PR edge. Indexability is
  all the marker claims — it is not a delivery claim: once plan
  `2026-09-04-docs-only-plan-marker-prs-derive-shipped` Phase 1 is deployed
  (authored 2026-09-04, not deployed as of that date), coord classifies a
  citation whose PR changed only plan documents as a *document citation*, which
  neither derives nor blocks `shipped`.

## Applies to "post-merge follow-up" and other non-plan-slug dispatches too

A second, distinct incident (2026-09-01, finding `a7e34abf-aaab-4f6d-978a-2f26742eed51`):
a session dispatched with "PR #N just merged, do the post-merge follow-up" —
no plan file, no plan slug, so this skill did not obviously apply — diagnosed a
gap in the merged PR and implemented a fix that duplicated another session's PR,
merged ~7h earlier, which had already closed the identical gap more completely.
The worktree was checked out at PR #N's own merge commit, and the session never
compared that commit against the CURRENT `origin/main` before diagnosing "what's
missing" — so a peer's already-landed fix, sitting a few commits ahead, was
invisible to it.

Steps 0, 1 and 5 are genuinely keyed on a plan **slug** and do not apply to this
dispatch shape. **Step 4 is not** — run it (the range-diff form above) for ANY
task that starts from "diagnose gaps in / complete the follow-up to X": a
just-merged PR, a shipped feature, a closed issue, before writing the diagnosis
or the fix. A merge commit named in a dispatch is a snapshot of main at *that*
moment, not a live pointer to main's current state, and coord's advisory
overlap tools (`declare_intent`, `who_is_working_on`) only see currently-declared
work — they are blind to a peer's fix that has already shipped and dropped out
of `conflict_check`'s unmerged-branch scan, exactly as in the #564/#565/#567
incident above, just discovered later (at PR-open time, via a CONFLICTING
`coord_pr_status` verdict) rather than never.

Steps 2/3/6 are not slug-dependent either (they run on globset paths, not a
plan's content) and remain worth running if you can derive globs some other
way — e.g. from the target PR's own changed-file list — though step 4 is the
one that would have caught this specific incident.

## Verification (incident replay)

- With the peer branch **open**, step 3 returns `conflicting_branches` naming it.
- With the peer branch **merged+deleted**, step 0 returns `held` (if
  concurrent — the `fork_risk` overlay is no longer part of the reserve
  response), step 1 reports the plan `shipped`, and step 4's plan-path search
  finds the merged citation.

Either way, the second agent stops **before** implementing.

## See Also

These references live in repos you may not have checked out
(`qontinui-dev-notes`, `qontinui-coord`); skip any whose repo is absent
under `<workspace-root>/`.

- `<workspace-root>/qontinui-dev-notes/plans/2026-06-13-coord-parallel-duplication-prevention.md` —
  the source plan (Layer A pre-flight + Layer B conventions + Layer C server work).
- `<workspace-root>/qontinui-coord/crates/coord/src/mcp/tools.rs` — `coord_reserve_resource`
  (`:3183`), `coord_conflict_check`, `coord_declare_intent` (single source of
  truth for the tool shapes above).
- `coord-pr-label` skill — declare cross-repo PR dependencies once you're cleared
  to proceed and have a PR open.
