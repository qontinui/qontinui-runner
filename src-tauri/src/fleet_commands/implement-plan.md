# Implement Plan

Execute an approved implementation plan end-to-end in a single session, without stopping between phases.

**Prerequisites:** A plan must already exist and be approved in the current conversation.

## Arguments
- `$ARGUMENTS` - Optional: specific notes or constraints for this implementation run

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
python D:/qontinui-root/qontinui-stack/scripts/resolve-plan-deps.py \
    D:/qontinui-root/plans/<this-plan>.md --json
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

1. Try `D:/qontinui-root/plans/<stem>.md` first (in-progress location).
2. If that doesn't exist, try `D:/qontinui-root/qontinui-dev-notes/plans/<stem>.md`
   (shipped archive).
3. If neither exists, the dep is **missing** — abort (see below).

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
- **Any dep file is missing** (neither directory has `<stem>.md`)
  → **abort the skill** with an actionable error before stamping anything.
  Example error text:
  ```
  Cannot start implementation: plan declares Depends-On: <stem>, but no
  matching plan file exists at:
    D:/qontinui-root/plans/<stem>.md
    D:/qontinui-root/qontinui-dev-notes/plans/<stem>.md

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

Do all three (they're fast and independent):

1. **The plan's own status block.** Re-read the top of the plan you're
   about to implement. If it already reads `SHIPPED` / `IN PROGRESS` (with
   a recent date + a different session marker) / `SUPERSEDED` / `OBSOLETE`,
   STOP and surface to the operator — another run already took it (Step 0.5
   covers the lifecycle rules, but check here *before* stamping so you
   don't race).
2. **Merged work on `main`.** For each repo the plan touches, scan recent
   history for the plan's stem, session tags, or distinctive symbols:
   ```bash
   git -C <repo> log origin/main -20 --oneline | grep -iE "<plan-stem keywords>"
   ```
   A hit means the plan (or a superset) may already be live — read the
   commit, and if it covers the plan's scope, surface to the operator
   rather than re-implementing.
3. **Open PRs.** Check for an open PR implementing the same plan:
   ```bash
   gh pr list --repo <owner/repo> --state open --search "<plan-stem keywords>"
   ```
   An open PR from another session means a live peer — coordinate rather
   than double-build.

If any surface shows the work is already done or in-flight, surface a
one-line summary + the evidence (commit SHA / PR number) via
`AskUserQuestion` (header `Already implemented?`, options: **Abort** /
**Proceed anyway** — e.g. the existing work is partial). If all three are
clean, proceed to Step 0.5. This is a read-only reconnaissance — it never
mutates anything and adds ~10s, far cheaper than building a redundant PR
that gets closed.

### Step 0.5: Stamp the plan as IN PROGRESS

Before any code changes land, edit the plan .md to update its status block to:

```markdown
> **Status: IN PROGRESS <YYYY-MM-DD>.** Implementation started by
> session <short session id or branch name>. Phase tasks: <N>. Started
> from <prior status — usually VETTED <date>>.
```

Rules:
- If the existing block is `Status: VETTED <date>`, replace it with the IN PROGRESS line above and reference the vet date in the body (`Started from VETTED 2026-05-02.`).
- If the existing block is `Status: DRAFT` or absent, add the IN PROGRESS block but warn the user in your first text turn that the plan was not vetted — give them a chance to abort and run `/vet-plan` first.
- If the existing block is `Status: PARTIAL` or `Status: NOT STARTED` (set by `/verify-plan-status`), replace it with the IN PROGRESS block and capture the prior state in the body's `History:` line. Don't run `/vet-plan` first unless the user asks — `/verify-plan-status` doesn't supplant a vet pass, but a recent NOT STARTED is also not a reason to re-vet.
- If the existing block is already `IN PROGRESS`, refresh the date and append your session marker — multiple agents may pick up the same plan; keep the trail.
- If the existing block is `SHIPPED` / `SUPERSEDED` / `OBSOLETE`, STOP — implementing a shipped plan is almost certainly a mistake. Confirm with the user before proceeding.

**Set the work-unit status directly.** IN PROGRESS / SHIPPED transitions drive
`unit_status` gates via coord's work-unit registry — there is no longer a
plan-ingest worker mirroring `qontinui-dev-notes/plans/` file edges into
`coord.plans`. Transition the work unit explicitly:
`POST $COORD_HTTP_URL/coord/work-units/<plan stem>/transition`
`{to_status:"in_progress", by_actor:"<session>"}` (the device-authed registry
route). The plan .md stamp + archive still apply (commit + push the stamped file
when archiving — the markdown artifact, its stamps, and the archive step all STAY);
only the coord-registry transition moved from "push a file edge" to "call the
transition route". (claude-config is NOT a coord sole-authority repo — its PRs
land via normal GitHub flow.)

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

**Skip-and-warn for non-coord environments.** If neither
`QONTINUI_MACHINE_ID` nor `~/.qontinui/machine.json` is available (e.g.
running on a developer laptop that isn't a registered qontinui device),
emit a single-line warning to the user (`⚠️ coord pre-flight skipped: no
machine_id available — running without claim coordination`) and proceed
without claims. This skill MUST remain usable in non-coord environments.

#### Resolution chain

For each phase you are about to launch:

1. **Plan-stem.** From the plan path (e.g. `D:/qontinui-root/plans/2026-05-18-agent-spawn-coordination.md`),
   take the filename without `.md` — `2026-05-18-agent-spawn-coordination`.
2. **Resource key.** `plan:<plan-stem>:phase:<phase-number>` —
   e.g. `plan:2026-05-18-agent-spawn-coordination:phase:3`.
3. **`machine_id`.** Env `QONTINUI_MACHINE_ID` first. Else read
   `~/.qontinui/machine.json` and parse the `"machine_id"` string value
   (UUID). DO NOT fabricate a value if neither source provides one —
   take the skip-and-warn path above.
4. **Coord HTTP base.** Env `COORD_HTTP_URL` first. Else
   `https://coord.qontinui.io`.

#### Session-id resolution (owner-token discriminator)

The phase claim's *holder identity* is a session-scoped **owner token**
(`<machine_id>:<agent_session_id>`), per plan
`2026-06-03-coord-session-scoped-claim-owner`. Sending `agent_session_id`
is what makes a SECOND `/implement-plan` session on the SAME machine see a
structured `held` conflict instead of silently taking over the first's
phase claim (the bug this guards against). Resolve a stable per-session
UUID ONCE, before the first acquire, and reuse it for every
acquire/heartbeat/release in this run:

```bash
AGENT_SESSION_ID="${QONTINUI_AGENT_SESSION_ID:-${CLAUDE_CODE_SESSION_ID:-}}"
```

`CLAUDE_CODE_SESSION_ID` is the harness session UUID — per-session-unique
and inherited by spawned phase agents (they run in the same Claude
session), so a child agent's heartbeat owner-token matches the parent's
acquire automatically. If both are empty (older Claude Code), omit the
field — coord's `None` fallback preserves today's machine-only behavior.
(The `coord-curl.sh` wrapper injects the same value with the same
precedence for any coord call that omits it; sending it explicitly here is
belt-and-braces and keeps the claim path independent of the wrapper.)

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
  against this claim via `/claims/heartbeat`). **Then cancel any pending
  continuation for this plan (takeover — see below)**, and proceed to
  launch the phase agent for this phase.
- `"held"` → another agent already holds the claim. **DO NOT launch
  the phase agent.** Enter the conflict resolution flow below.
- `"topic_conflict"` / `"topic_unknown"` / `"invalid_topic"` → surface
  the error verbatim to the operator and abort the skill. These are
  not expected from the call shape above but handle defensively.

#### Cancel a pending continuation on takeover

Taking the phase claim directly means THIS session is doing the work a
`unit_ready` gate's continuation may have queued as a fresh runner-terminal
spawn. Leaving that pending continuation alive means the runner spawns a
**redundant terminal** on its next WS reconnect (its in-process dedupe set is
forgotten across restarts, and the pending row survives up to 24h). So,
**best-effort, right after the FIRST phase claim of this run succeeds** (do it
once per run, not per phase):

1. Resolve the `work_unit_id` for this plan-stem via
   `GET $COORD_HTTP_URL/coord/work-units/<plan-stem>` (or the upsert used elsewhere).
2. Query the work unit's gates for a pending continuation:
   `GET $COORD_HTTP_URL/coord/gates?work_unit_id=<id>` → rows where
   `continuation_dispatched_at != null ∧ continuation_consumed_at == null ∧
   continuation_cancelled_at == null`.
3. For each such gate, fire (operator/`TenantId` bearer — same auth layer as
   mute/reopen; tenant derives server-side):

   ```bash
   curl -fsS -X POST "$COORD_HTTP_URL/coord/gates/<gate_id>/continuation-cancel" \
     -H "Content-Type: application/json" \
     -d "$(cat <<EOF
   { "cancelled_by": "$AGENT_SESSION_ID", "reason": "taken over by session $AGENT_SESSION_ID" }
   EOF
   )"
   ```

This is **best-effort and MUST NOT block** the phase launch: a non-2xx, a 404
(nothing was ever dispatched — nothing pending), or a network failure is fine —
narrate it and proceed. A **409 `already_consumed`** means a spawn already
happened: report it honestly (do not claim a clean takeover) but still proceed
with this session's work. Narrate the outcome either way — "cancelled pending
continuation `<gate_id>`" or "no pending continuation to cancel".

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
    `http://127.0.0.1:<port>/coord-mcp` url + `X-Coord-Mcp-Proxy-Key`
    header): `curl GET <proxy_base>/claims/by-resource?kind=phase&key=<rk>`
    with the `X-Coord-Mcp-Proxy-Key: <nonce>` header (the runner injects a
    live device JWT).
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

After each phase agent reports — whether success OR failure — release
the claim:

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

The release endpoint is idempotent — a `"not_held"` response is fine
(heartbeat-based eviction may have already cleaned up if the phase ran
longer than the claim's TTL with no heartbeat). Treat release as
try/finally semantics: release MUST fire even on phase-agent failure,
even on `/implement-plan` skill abort. If the operator chose Abort in
the conflict-resolution flow, release every claim this session already
acquired for earlier phases before exiting.

Phase claims have a 7200s (2 hour) default TTL per `claims.rs:121`.
For phases expected to exceed 2 hours, the spawned phase agent should
heartbeat via `POST $COORD_HTTP_URL/claims/heartbeat` every TTL/3 seconds.
This skill currently does NOT auto-heartbeat between phase launches;
phases under 2 hours run safely on the initial acquire alone.

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

#### Resolution chain (same identity sources as Step 0.6)

1. **`device_id`.** Env `QONTINUI_MACHINE_ID` first. Else read
   `~/.qontinui/machine.json` and parse the `"device_id"` field (the
   canonical name post-unified-devices); fall back to `"machine_id"`
   if present (legacy shape). The coord wire-field name is `device_id`
   regardless of which local key supplied the UUID.
2. **`current_repo`.** `basename "$(git rev-parse --show-toplevel)"`
   from the worktree the skill is executing in.
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

When Step 6 (SHIPPED stamp + archive) completes successfully, POST one
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
resource**, it must reserve that resource through coord first — so two
concurrent sessions don't hand-pick the same `down_revision` / registry
slot and fork `main`. This is the read-side of the auto-reconcile +
handshake layer (plan
`2026-06-02-coord-conflict-autoreconcile-and-agent-handshake`); it pairs
with the auto-rebase that re-points a loser when a fork *does* slip
through, and with Plan A's CI gate that enforces the reservation existed.

**Mandatory-reserve scope.** A resource is mandatory-reserve iff coord has
a **registered `SemanticResource` grammar** for it. Today that is exactly:

- **Alembic migration heads** — any phase that authors a new migration
  (a `down_revision` pick). coord owns chain succession now: ask for a
  **slot** and coord ASSIGNS your `down_revision`. Reserve it with
  **`coord_migration_reserve`** (HTTP: `POST $COORD_HTTP_URL/coord/migrations/reserve`),
  NOT `coord_claim_acquire` / `coord_reserve_resource`. The old
  `kind=alembic_revision` claim is retired and returns **410**; the CI gate
  matches your PR against your reservation and auto-binds it (no
  `coord.claims_audit` claim to keep alive).
- **The MCP tool registry** — any phase that adds/renumbers a tool
  registration or a grammar-tracked count assertion. Resource:
  `mcp-tool-registry:<mount>` (the registry mount, e.g. `phase11` — not
  the repo), reserved via `coord_reserve_resource`.

Other tracked-but-not-yet-grammared resources (enums, lockfiles) are
**advisory** — reserve if convenient, but the CI gate is not (yet) keyed
on them. The grammar registry is the single source of truth for "must
reserve"; when a new grammar ships, that class becomes mandatory
automatically — do not maintain a separate hand-edited list here.

**Scan + inject.** Before launching each phase agent (alongside the
Step 0.7 UI-Bridge scan), check whether the phase's touched-files /
description authors a mandatory-reserve resource. If so, include this
block in the agent prompt:

> **Reserve-before-author handshake.** Before you author this change,
> reserve the resource over MCP — the call differs by resource class:
>
> **Migration (alembic):** ask coord for a slot —
> `POST $COORD_HTTP_URL/coord/migrations/reserve {"repo":"<repo>","revision":"<your-new-rev-id>","machine_id":...,"agent_session_id":...}`
> (MCP: `coord_migration_reserve` when available). The response ASSIGNS your
> `down_revision` — use it verbatim; never compute the head from a local
> checkout. `position > 1` means you're stacked behind in-flight migrations
> (fine — author against the assigned head). No heartbeat, no claim juggling:
> push your PR and the CI gate auto-binds it; merge releases the slot. If a
> predecessor expires/withdraws, coord re-points you — the gate (and a PR
> comment) tells you the corrected `down_revision`; update the file and
> re-push. Withdraw an abandoned slot:
> `POST /coord/migrations/:id/withdraw {"reason":...}`. The old
> `kind=alembic_revision` claim returns 410.
>
> **Tool registry / enum / lockfile:** call
> **`coord_reserve_resource(kind, name)`** — `kind` is the lowercase-kebab
> class (`mcp-tool-registry`, `enum`, `lockfile`); `name` is the instance
> (e.g. the mount `phase11`). coord keys a `semantic_resource` claim on
> `<kind>:<name>`.
>
> **Branch on the `coord_reserve_resource` result (tool-registry / enum /
> lockfile):**
> - **`Granted`** (or `claimed` with no `forking_siblings`) → you hold the
>   reservation; proceed to author.
> - **`Held { holder }`** → another agent owns it. **Do NOT hand-pick a
>   value.** Wait for release (poll `coord_claim_check`) or coordinate with
>   the holder, then re-reserve.
> - **`ForkRisk { siblings }`** / a non-empty **`forking_siblings`** →
>   sibling PR(s) are already racing this resource. Do **not** pick a value
>   off the old head — chain off the current head or coordinate with the
>   named siblings, so you don't author the fork.
>
> For the tool-registry path, heartbeat (`coord_claim_heartbeat`) if
> authoring takes a while; release (`coord_claim_release`) once your commit
> that claims the slot lands. (Migrations need none of this — the
> reservation binds to your PR and is released by merge.)

The semantic-resource tools live on coord's `phase11` MCP mount
(`coord_claim_acquire/heartbeat/release/check`, `coord_reserve_resource`);
migration reservations use `coord_migration_reserve` / `_bind_pr` /
`_withdraw` / `_queue`. If a phase agent has no MCP access to coord, fall
back to the HTTP API: migrations use
`POST $COORD_HTTP_URL/coord/migrations/reserve` (coord assigns the
`down_revision`); other semantic resources use
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
   multiple Agent tool calls in a single message.
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
(`POST /claims/acquire` with `kind=phase, resource_key=plan:<stem>:phase:<n>`).
If `"held"`, resolve the conflict (abort/wait/steal) before proceeding to
the Agent launch. When launching phases in parallel, pre-flight each
phase's claim sequentially first (parallel acquires against distinct
resource keys are safe but easier to surface conflicts on linearly),
then launch the surviving phase Agents in parallel.

**Coord activity UPSERT (per Step 0.6.5).** Immediately AFTER each
phase's `/claims/acquire` succeeds and BEFORE launching that phase's
Agent, fire the Step 0.6.5 phase-launch UPSERT with `details.phase`
set to that phase's `<n>/<total>`. Failure is non-fatal (warn-and-
continue per Step 0.6.5 rules). For parallel launches: fire the
UPSERTs sequentially in launch order, then launch the Agents in
parallel.

**Reserve handshake (per Step 0.7.5).** If a phase authors a
mandatory-reserve semantic resource (a new migration / a tool-registry
change), include the Step 0.7.5 reserve-before-author block in that
phase's Agent prompt so the agent reserves the resource (and, for
migrations, uses the `down_revision` coord assigns) before authoring —
never hand-picking a colliding value.

**Edit-effect loop (per Step 0.7.6).** Include the Step 0.7.6
predict→gate→verify block in EVERY phase Agent prompt so the agent
predicts before its first edit, surfaces any `escalate` risk_factors to
you, and verifies after its commit. Best-effort — never blocks a launch.

**Launch independent phases in parallel** using multiple Agent tool calls in a single message. Only serialize phases that have true dependencies on each other.

Each agent should:
- Implement the phase completely
- Run type checks and lints (`cargo check`, `npx tsc --noEmit`, `ruff check`, etc.)
- Fix any errors or warnings
- Report back: files changed, what was implemented, any issues found and fixed

**Claim release (per Step 0.6).** AFTER each phase Agent returns —
success OR failure — release that phase's claim via
`POST /claims/release`. Treat as try/finally: release MUST fire even on
agent failure or exception. On skill abort, release every claim this
session acquired for any phase before exiting.

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

Use `/clean-commit` or commit manually. Do NOT include AI attribution.

**Cooperative abort-report (commit-action effect signatures §6.2).** If a
`git commit` is REJECTED by a pre-commit hook (non-zero exit), forward the
reason to coord before fixing + retrying:
`bash D:/qontinui-root/.claude/scripts/report-commit-abort.sh "<captured hook output>"`
— best-effort, fail-open; it never edits git or blocks. `/clean-commit` Phase 4
does this automatically; do the same on a manual commit. Never `--no-verify` to
bypass the hook — that defeats both the hook and the supervision signal.

### Step 5: UI Bridge Improvement Plan (if manual testing was performed)

If manual testing was performed in Step 2, create a plan (using EnterPlanMode) for UI Bridge improvements based on friction encountered during testing. This plan is for a future session — do not implement it now.

### Step 6: Mark the plan done and archive it

Once Steps 1–5 land cleanly:

1. **Stamp a status block at the top of the plan .md** (just below the H1 title) summarizing what shipped — applying the single-stamp invariant from Step 0.5 (delete the existing IN PROGRESS block, write SHIPPED in its place):
   ```markdown
   > **Status: SHIPPED <YYYY-MM-DD>.** <one-paragraph summary of what's
   > live and where to find it — repo + key commit SHAs at minimum>.
   ```
   Keep it short — 3–6 lines. List the canonical commit SHAs (one per repo touched). If there's a follow-up plan with open items, name it.

2. **Move the plan from in-progress to the completed archive.** Workspace convention:
   - In-progress: `D:/qontinui-root/plans/` (workspace root, not git-tracked).
   - Completed: `qontinui-dev-notes/plans/` (git-tracked archive).

   Use a plain `mv plans/<name>.md qontinui-dev-notes/plans/<name>.md`. The plan's followup file (if any) stays in whichever location matches its own state — completed plans go to dev-notes, plans with open items stay at the workspace root.

3. **Commit the archived plan in dev-notes.** Single commit with message like `plans: archive <plan-name> as shipped (<commit-sha-summary>)`. Push. The archive commit is the durable record of the markdown artifact; it no longer drives the coord registry (the ingest worker is gone). Transition the work unit's status to SHIPPED explicitly via `POST $COORD_HTTP_URL/coord/work-units/<plan stem>/transition {to_status:"shipped", by_actor:"<session>"}` so any `unit_status` gate clears — don't skip either.

4. **If the plan was already authored inside `qontinui-dev-notes/plans/`** (e.g. it was vetted there from the start), skip the move; just edit the status block in place and commit.

5. **Clear coord activity status (per Step 0.6.5).** Fire the final
   clearing `POST /coord/status` documented in Step 0.6.5 with
   `current_task: null` so the dashboard tile stops showing this plan
   as in-flight. Failure is non-fatal — `prune_stale()` TTLs the row
   within an hour.

This step is mandatory. Plans without a status stamp lose context within weeks; plans left in the in-progress dir after they ship clutter triage and confuse future agents into thinking work is still pending.

### Step 6.5: Offer to register a coord gate for any deferred/blocked phase

*(canonical spec: `_gate-registration` — keep copies in sync)*

This fires whenever a phase is **deferred or blocked on an observable condition**
— a phase agent failed/aborted on something coord can watch (a PR must merge, a
deploy must go healthy, CI must go green, a metric must cross a threshold, a time
window must elapse, an operator must approve), OR Step 6 records a "follow-up
plan with open items" that waits on such a condition. A deferral with no
observable trigger (open-ended TODO) is NOT a gate — skip those.

- **Default = explicit offer.** Ask via `AskUserQuestion` (header `Register
  gate?`, options Register / Skip), showing the derived anchor, predicate kind,
  and human-readable condition. Under opt-in auto mode (env `QONTINUI_AUTO_GATE=1`)
  register WITHOUT asking and report what was registered (gate_id + predicate).
- **Anchor (zero user input):** `work_unit_id` from
  `POST $COORD_HTTP_URL/coord/work-units/upsert` with the plan stem as `slug`
  (or `GET /coord/work-units/<slug>`); `phase_name` from the phase heading. Anchor
  = (work_unit_id, phase_name). Claim-bound deferrals use the claim-anchored shape
  (`claim_kind`+`resource_key`) instead.
- **Register:** prefer MCP `coord_register_gate` (kinds: `pr_merged`,
  `deploy_healthy`, `claim_terminal`, `operator_approval`, `ci_green`,
  `ref_exists`, `metric_threshold`, `time_elapsed`, `unit_ready`,
  `migration_at_head`, `infra_drift_clear`, `file_exists`, `sql_count`,
  `unit_status`, `gate_cleared`; optional
  `continuation_prompt` e.g. `run /implement-phase <stem> "Phase N"` for
  auto-resume). **HTTP fallback** when MCP is unavailable — for a work-unit-anchored
  gate it is a **TWO-call flow** (the register route never upserts the work unit,
  and 404s `work_unit_not_found` if the slug isn't present): (1) first
  `POST $COORD_HTTP_URL/coord/work-units/upsert {slug, title?, status?}` and
  capture `work_unit_id` from the response; (2) then
  `POST $COORD_HTTP_URL/coord/work-units/<slug>/register-gate` with a raw device JWT
  (resolves tenant from the device JWT; body omits `slug`/`work_unit_id`, which come
  from the path + the predicate). All work-unit routes are device-authed
  (`require_jwt`), so a device session reaches them directly; the operator-only
  `/coord/gates/register` is not used for a work-unit anchor. For a claim-anchored
  gate (no work unit) or with no device identity, fall back to
  `POST $COORD_HTTP_URL/coord/gates/register` (default `https://coord.qontinui.io`).
  Tenant derives server-side — never pass it.
- **`clearance_audience`:** set `agent` for agent-verifiable facts ("/vet-plan
  was run", "crate exists + tests green", "a dual run emitted evidence") so the
  session that completes the work can attest the gate itself; set `operator` for
  business/judgment/strategy or on-page-human-verification gates. Default is
  `operator` if omitted; the sensitive-work rule always forces `operator`.
- **Predicate choice:** wait-on-PR → `pr_merged`; wait-on-deploy →
  `deploy_healthy`; wait-on-CI → `ci_green`; burn-in / wait-N-days →
  `time_elapsed`; metric condition → `metric_threshold` (explicit `labels` — e.g.
  `coord_ci_runner_count` MUST filter `{status:"idle"}`); a vetted plan that is
  ready, dispatchable work → `unit_ready` `{work_unit_id, ready_status}` (**NOT**
  `operator_approval` — `operator_approval` is for genuine human decisions, not a
  work queue); schema/alembic-at-head → `migration_at_head` `{schema}`; infra drift
  cleared → `infra_drift_clear`; a repo file/workflow existing → `file_exists`
  `{repo,path,on_ref?}` (contents, not refs); a coord data count crossing a bound
  → `sql_count` `{query_id,op,n}` (whitelisted `query_id`, never raw SQL); an
  umbrella work unit reaching a status → `unit_status` `{work_unit_id, status}`;
  another cross-anchor gate clearing → `gate_cleared` `{gate_id}`;
  needs-human → `operator_approval`. Anything **security / credential / billing /
  strategy-sensitive** registers as `operator_approval` + notify — never an
  auto-resuming gate, never silently auto-registered.
- **Masked-tool honesty:** per-agent MCP allow-set curation can mask
  `coord_register_gate` as unknown (coord `mcp/mod.rs`). If the call fails as
  unknown/method-not-found, report **"gate NOT registered — coord_register_gate
  not in this session's tool allow-set"** and fall back to HTTP (or surface to the
  operator). NEVER report a gate registered without a returned `gate_id`.
- The optional plan-file `## Gates` block is a **local convenience mirror only** —
  coord is the source of truth; never require it, never read it back as
  authoritative.

**Attest-on-completion (close the loop).** When this run instead COMPLETES work
that a registered gate was watching (e.g. a deferred phase that an earlier
session gated now finishes), it MUST attest that gate — otherwise an agent-fact
gate rots open until a human clicks it.

- **Find the gate:** by the `gate_id` recorded at registration, or by lookup
  `GET $COORD_HTTP_URL/coord/gates?work_unit_id=<id>&phase_name=<name>` — the OPEN
  gate whose condition the completed work satisfies.
- **Attest:** prefer MCP `coord_attest_gate` (pass `gate_id` — works from a device
  session since attest takes no plan upsert); fall back to the device loopback
  forwarder `POST http://127.0.0.1:{runner_port}/coord-mcp/gates/{gate_id}/attest`
  (header `X-Coord-Mcp-Proxy-Key`, no body bearer — maskless fallback), then the
  direct device-authed `POST $COORD_HTTP_URL/coord/gates/:gate_id/attest`. Tenant
  derives server-side — never pass it. Legal only on an OPEN `operator_approval`
  gate with `clearance_audience = 'agent'` in the caller's own tenant; coord flips
  it to `cleared` and fires the same fanout as operator approve.
- **Masked-tool honesty:** if `coord_attest_gate` is unknown/METHOD_NOT_FOUND it
  isn't in this session's allow-set → fall back to the HTTP attest route. NEVER
  claim a gate attested without a returned cleared `gate_id`.
- **Honesty:** NEVER report a deferred item as done without EITHER a cleared
  `gate_id` OR an explicit "gate not found" note.

**Continuation cancel (refresh + takeover).** A CLEARED `unit_ready` gate may have
dispatched a pending runner-terminal continuation. If you **re-register** a gate
for the same work-unit/anchor, or this run **directly takes over** the work a
pending continuation was queued for (the active takeover wiring lives in Step 0.6),
first cancel that pending continuation so the runner does not spawn a redundant
terminal: find it via `GET $COORD_HTTP_URL/coord/gates?work_unit_id=<id>` (rows with
`continuation_dispatched_at != null ∧ continuation_consumed_at == null ∧
continuation_cancelled_at == null`), then
`POST $COORD_HTTP_URL/coord/gates/:gate_id/continuation-cancel`
`{cancelled_by, reason}` (`TenantId` auth, on the operator/`TenantId` path,
unchanged). Best-effort, never blocking:
404 = nothing pending; **409 `already_consumed` = a spawn already happened, report
it honestly** rather than claiming the cancel landed. Narrate the cancelled
`gate_id`. (canonical spec: `_gate-registration` → "Continuation cancel + refresh".)

## Rules

- **Phases run as Agents, not Skill calls** — this keeps implementation work out of the main context
- **Never stop between phases** — the entire plan executes in one session
- **Complete ALL work** — never skip tasks due to size or complexity
- **Fix, don't report** — fix issues immediately, don't just list them
- **Parallel by default** — launch independent phases concurrently; only serialize when there are true data dependencies
- **Edit work runs in an allocated worktree, never the primary checkout** — before launching a phase Agent that will `Edit` / `Write` / run `git` against a coord-registered repo, the coordinator creates an isolated git worktree for that repo directly — `git -C <repo> worktree add -b <branch> <workspace-root>/<repo>-wt-<slug> origin/main` — and passes that path as the Agent's working directory. (The former HTTP face, `POST /agents/allocate-local`, was removed as dead code in runner #443; the in-process `agent_worktree::isolated_edit` facade serves runner-internal spawns only.) The Agent treats that path as its repo root; every edit lands there, never in the operator's primary checkout. Sibling to the `/manual-test` "never touch the primary runner" rule — same shape (don't share the primary), different substrate (git worktree vs supervisor temp runner). Remove the worktree (and its isolated CARGO_TARGET_DIR, kept OUTSIDE the worktree) after the work ships. **Why:** see `plans/2026-05-28-isolate-session-edit-work-in-worktrees.md` — the proximate cause was two concurrent skill chains editing the same primary checkout simultaneously.

## Implementation Notes

$ARGUMENTS
