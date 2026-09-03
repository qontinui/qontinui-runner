---
name: preflight
description: Reserve-before-you-code protocol — run at the START of any plan or task, before writing the first line. Reserves the plan in coord, checks the durable plan registry for already-shipped work, derives concrete globs, runs conflict_check + the merged-branch out-of-band search, declares intent with the plan slug, and acquires + heartbeats file-glob claims. If any overlap signal is non-empty, coordinate instead of proceeding. Prevents two agents from silently duplicating the same plan (the #564/#565-vs-#567 incident).
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

HTTP fallback (when the coord MCP tool is unreachable):

```bash
curl -s -X POST "$COORD_HTTP_URL/claims/acquire" \
  -H "Content-Type: application/json" \
  -d '{"kind":"semantic_resource","resource_key":"plan:<plan-slug>"}'
```

This mints a `semantic_resource:plan:<slug>` claim with an atomic SET-NX and
returns exactly one of:

- **`granted`** — you are the first agent on this plan. Proceed (run the rest of
  the checklist anyway — the reserve is the live layer, not the whole guard).
- **`fork_risk`** — racing sibling PRs detected but no hard holder. Proceed, but
  treat the named siblings as overlap candidates and inspect them in step 4.
- **`held`** — a peer is **already implementing this plan** (the response carries
  the holder identity). Do **not** start. Coordinate (hand off / sequence behind
  them) or stop.

> Verified: `coord_reserve_resource` exists at
> `qontinui-coord/src/mcp/tools.rs:3183` and returns granted / held / fork_risk.

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
qontinui-coord/src/mcp/tools.rs
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

### 5. `declare_intent` with the plan SLUG as `work_unit_id`

```
coord_declare_intent(
    work_unit_id="<plan-slug>",
    intent_globs=["<glob>", "<glob>", ...],   # the explicit list from step 2
)
```

Also set `plan_slug` on the session.

Then record the same plan context **on the worktree**, so the WIP-custody Stop
hook stamps it into every custody record this session writes:

```bash
qontinui-claude-config/scripts/custody-intent-write.sh <worktree> \
    plan_slug=<plan-slug> \
    intent="<one-line description of the work>"     # omit if you have none
```

It writes `$GIT_DIR/qontinui-custody.intent` as `KEY=value` lines — the exact
form `wip-custody-record.sh` already parses. Without it the custody record's
`work_unit_id` / `plan_slug` / `intent` are **null**, and the runner surfaces
that read them (worktree census, on-demand attribution) can only say UNKNOWN.

- **Pass `plan_slug=`, not `work_unit_id=`.** This step performs no work-unit
  upsert and gets no UUID back — the `work_unit_id` argument above *is* the plan
  slug, so a `work_unit_id=` here could only ever repeat the slug under the
  wrong key. The UUID is written by the lifecycle step that actually mints one.
- Path is **workspace-relative** — run it from the workspace root (or prefix
  `$QONTINUI_ROOT/`). Never hardcode an operator-local absolute path.

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
- **Heartbeat** the claims for the work's lifetime. The incident's claims were
  90s TTL and never heartbeated → they expired and left the work unprotected.

### 7. If any signal is non-empty → coordinate, don't proceed

`held` in step 0, a non-empty `conflicting_branches` / `claim_holders` /
`overlapping_peers` in step 3, or a matching PR/commit in step 4 all mean the
same thing: **stop and coordinate.** Request a handoff, yield, sequence behind
the peer, or pick a different work unit. Only an all-clear sweep lets you write
the first line.

## Layer B conventions (apply these alongside the checklist)

- **Repo-default correlation topic.** All agents on repo `R` use the
  deterministic topic `repo:<R>` (e.g. `repo:qontinui-runner`) so `coord_orient`
  sees every peer on the repo without both sides guessing the same ad-hoc string.
  Reserve ad-hoc sub-topics for genuinely-scoped sub-efforts, layered on top.
- **Always pass explicit globs** to `declare_intent`. An empty `intent_globs`
  return is a retry trigger, not a no-op (step 5).
- **Claim keys in globset path form**, consistent with what `conflict_check`
  scans (step 6).
- **Cite the plan path in every commit body and PR body** (e.g.
  `Plan: plans/<plan-slug>.md`) so the work is reliably indexable by the
  merged-branch search in step 4 and the durable plan→PR edge.

## Verification (incident replay)

- With the peer branch **open**, step 3 returns `conflicting_branches` naming it.
- With the peer branch **merged+deleted**, step 0 returns `held`/`fork_risk` (if
  concurrent), step 1 reports the plan `shipped`, and step 4's plan-path search
  finds the merged citation.

Either way, the second agent stops **before** implementing.

## See Also

These references live in repos you may not have checked out
(`qontinui-dev-notes`, `qontinui-coord`); skip any whose repo is absent
under `<workspace-root>/`.

- `<workspace-root>/qontinui-dev-notes/plans/2026-06-13-coord-parallel-duplication-prevention.md` —
  the source plan (Layer A pre-flight + Layer B conventions + Layer C server work).
- `<workspace-root>/qontinui-coord/src/mcp/tools.rs` — `coord_reserve_resource`
  (`:3183`), `coord_conflict_check`, `coord_declare_intent` (single source of
  truth for the tool shapes above).
- `coord-pr-label` skill — declare cross-repo PR dependencies once you're cleared
  to proceed and have a PR open.
