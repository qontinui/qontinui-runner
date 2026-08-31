---
description: Session-close protocol for a blocked agent — register a typed coord gate for any work you are stopping incomplete-because-WAITING on an observable condition, so the blocker becomes a watched gate instead of a silent stall.
argument-hint: "[brief description of what you are blocked on]"
allowed-tools: Read, Bash, Glob, Grep, ToolSearch
---

# Blocked — emit-on-block (session-close protocol)

You are about to stop work that is **not finished, because it is WAITING on an
observable condition** — something coord can later watch and detect (a PR
merging, a deploy going healthy, CI going green, a metric crossing a threshold,
a schema reaching head, a file appearing, a SQL count crossing a bound, a plan
reaching a status, another gate clearing, a time window elapsing, or an operator
approving).

**Before you close, you MUST register a typed coord gate** so coord watches the
condition and can auto-resume (or notify) after every session closes. A blocked
item that just sits in your report rots; a registered gate is a durable,
tenant-scoped, fleet-wide observation that survives session death.

This is the active half of coord gate auto-identification (harvest is the passive
half). The canonical spec for the registration mechanics is `_gate-registration`
— this skill is the canonical **session-close procedure** that invokes it. Keep
the two in sync.

## When this fires (and when it does NOT)

Fire `/blocked` when you stop work and the reason you can't finish is an
**observable trigger** — a condition with a concrete, machine- or
operator-detectable flip.

**Do NOT register a gate** for a blocker with **no observable trigger**:

- an open-ended TODO ("clean this up someday"),
- a product / scope / strategy call with no concrete detectable event,
- a vague "needs more thought" / design question.

A gate models **"watch until it flips, then act once."** A blocker that has
nothing to watch is NOT a gate — **leave it in your report** as a plain blocker.
Do not invent a predicate to force-fit it; that pollutes the gate registry with
gates that never clear.

## Step 1 — Pick the predicate kind (the forcing function)

`coord_register_gate` deserializes its input into a **typed `GatePredicate`** and
**rejects kind-less prose** — you cannot register "wait until the thing is ready"
as free text. Map what you are waiting on to exactly one kind:

| What you are waiting on | Predicate kind | Shape / notes |
|---|---|---|
| A PR merging (non-coord-orchestrated repo) | `pr_merged` | identify the PR (`repo` + `pr` number); **never fires on a coord-orchestrated repo** (ff-land closes with `mergedAt:null`) — use `commit_live` |
| Work landing on main of a **coord-orchestrated repo** | `commit_live` | `{repo, commit_sha, on_ref?}` — ancestor-of-main check; anchor a **post-land main SHA** (or use `unit_status` — **not `file_exists`, which is broken**), NEVER the pre-land branch-head SHA — rebase-land rewrites SHAs and the gate rots open (gate `c14d103c`, 2026-07-11) |
| A deploy going healthy | `deploy_healthy` | the service/env that must be healthy |
| A claim going terminal (released/expired) | `claim_terminal` | claim-anchored, not plan-anchored (`claim_kind`+`resource_key`) |
| A human decision / judgment | `operator_approval` | `{prompt}` — notify-only; the only free-text-ish kind, and the human escape hatch |
| CI going green | `ci_green` | the ref/workflow that must pass |
| A git ref/tag appearing | `ref_exists` | the ref that must exist (refs, **not** file contents) |
| A metric crossing a threshold | `metric_threshold` | `{metric, labels, op, value, window_secs?}` — name `labels` explicitly (e.g. `coord_ci_runner_count` MUST filter `{status:"idle"}`) |
| A time window / burn-in elapsing | `time_elapsed` | `{since (default now), duration_secs}` |
| A vetted plan that is ready, dispatchable work | `unit_ready` | `{work_unit_id, ready_status}` — auto-clears when the unit reaches `ready_status` + sibling gates cleared; **NOT** `operator_approval`. Transition the unit FIRST and set `ready_status` to what landed (`vetted`, else the Free fallback `vetted_unattested`) — a hardcoded Attested value on a unit you own never clears (canonical: `_gate-registration`) |
| A schema/alembic reaching head | `migration_at_head` | `{schema}` — delegates to the live schema observer (`applied_head == chain_head`) |
| Infra drift / active-negation clearing | `infra_drift_clear` | `{}` — delegates to the live infra observer (no active negation) |
| A repo **file / workflow / migration file** existing | ⛔ `file_exists` — **KNOWN BROKEN 2026-08-05, do not register one** (403s fleet-wide on the contents API, control-probed; the gate can never clear). Use `commit_live` with a post-land SHA, or `unit_status`. | `{repo, path, on_ref?}` — file **contents/presence**, unlike `ref_exists` |
| A coord **data count** crossing a bound | `sql_count` | `{query_id, op, n}` — `query_id` is a **whitelisted named query** (`devices_null_tenant` \| `open_gates` \| `draft_plans`), never raw SQL |
| An umbrella plan (work unit) reaching a status | `unit_status` | `{work_unit_id, status}` — reads the work unit's `status` (e.g. `shipped`/`archived`); distinct from `unit_ready` (ready + siblings) |
| Another, cross-anchor gate clearing | `gate_cleared` | `{gate_id}` — composition; same-anchor AND-of-gates is already implicit |

`sql_count.query_id` is a **typed enum, not a SQL string** — the same
typed-not-free-text discipline as the predicate itself. If you need a count that
isn't one of the three whitelisted queries, that is a "new predicate shape
needed" note (see Step 5), not a reason to smuggle SQL.

**No kind fits?** Then either (a) it is genuinely a human decision → register
`operator_approval{prompt}` with `clearance_audience: operator`, OR (b) it has no
observable trigger at all → it is **not a gate**, leave it in the report. You may
ALSO, when a real observable trigger exists but no current kind expresses it,
register `operator_approval{prompt}` as the interim gate AND add to your report:
**"new predicate kind needed: `<proposed shape>`"** so the vocabulary can grow.
You may never register prose as an evaluable predicate.

## Step 2 — Derive the anchor (zero user input)

Every gate needs an anchor — one of two shapes:

- **Plan-anchored (the usual case):** `(work_unit_id, phase_name)` — a plan tracked
  as a work unit.
  - `work_unit_id` — `POST $COORD_HTTP_URL/coord/work-units/upsert` with `{ "slug":
    "<plan-stem>", "title": "<plan H1>" }` (idempotent on slug; slug = plan
    filename stem, no `.md`/path) → capture `work_unit_id` (a UUID) from the
    response, OR `GET $COORD_HTTP_URL/coord/agent-work-units/<slug>` to read an
    existing id (the **device-authed** read door — the operator
    `GET /coord/work-units/<slug>` is `TenantId`-only and 403s a device JWT).
    The `unit_ready`/`unit_status` predicates take this UUID, not the slug.
  - `phase_name` — the phase heading text the block belongs to (or the plan title
    / section heading for a whole-plan deferral).
- **Claim-anchored** (only when the blocker is bound to a specific coord claim,
  not a plan phase): `(claim_kind, resource_key)` — e.g. "resume when this
  alembic-head claim goes terminal" → `claim_kind`+`resource_key` with a
  `claim_terminal` predicate.

`$COORD_HTTP_URL` defaults to `https://coord.qontinui.io`. Tenant always derives
server-side from the session JWT — **never pass a tenant argument.**

## Step 3 — Choose `clearance_audience`

Declare WHO is expected to clear the gate. It governs whether a later agent
session can attest the gate itself versus whether only an operator can:

- **`agent`** — an **agent-verifiable fact** the session that later completes the
  work can attest (e.g. "/vet-plan was run", "the crate exists + its tests are
  green", "the migration applied cleanly", "a dual run emitted evidence"). The
  completing session closes the loop via `coord_attest_gate` instead of waiting
  on a human click.
- **`operator`** — needs **business / judgment / strategy** input, or **on-page
  human verification**, or anything **security / credential / billing /
  strategy-sensitive**. Only an operator clears it.

**Default is `operator`** when omitted (the safe default). **Sensitive work
(security / credential / billing / strategy) ALWAYS registers as
`operator_approval` with `clearance_audience: operator` and notify-only** — never
an auto-resuming observation gate, never an `agent`-attestable one, even under
auto mode. When in doubt whether a blocker is sensitive, treat it as sensitive.

**Also pass `gate_class` — LIVE, and `/blocked` should classify BY DEFAULT.**
coord PRs #1246/#1249 and web #872 landed; the round trip is verified in
production (2026-08-03). A blocked-work gate is precisely where clearance
authority matters, so pick a class rather than omitting reflexively:

- **`security-surface`** — the deferred work this gate guards would itself fire
  a `security-and-autonomy` path glob or content trigger. You have already made
  that match to decide how to implement; reuse the answer. Note that the
  Sensitive-work rule above is stricter and independent — it forces
  `clearance_audience: operator` regardless of class.
- **`ops-confirm`** — you are blocked on a deploy, sweep, migration, or config
  application landing.
- **`routine-review`** — a mechanical follow-up another session can judge from
  the diff alone.
- **Omit** only when none of the three genuinely applies. Omitting is safe and
  is never a loophole; a guessed class is worse than none.

It feeds the per-tenant clearance-authority matrix that decides who may later
attest/reject the gate. NULL or an unmatched class falls in the default bucket
(never more permissive than today), and this tenant has zero configured
`gate_clearance` rules as of 2026-08-03, so behavior is byte-identical to today
until the operator authors one. (Canonical: `_gate-registration` →
"`gate_class`".)

## Step 4 — Auto-resume (`continuation` / `continuation_prompt` / `continuation_spawn`)

**Gate registration is CONTINUATION-LESS by DEFAULT** (canonical:
`_gate-registration` → "Continuation policy") — by default you omit the
continuation entirely: **no `continuation` and no `continuation_prompt` (MCP
`coord_register_gate`), and no `continuation_spawn` (HTTP `register-gate`)**. All
three spellings are the SAME knob — coord materializes both MCP fields into the
DB's `continuation_spawn` and both spawn — so "omitting `continuation_spawn`"
while still passing `continuation_prompt` produces exactly the duplicate,
parallel run the default exists to prevent. The default is **omission**
(`continuation_spawn` NULL), *not* the typed `{"action":"notify_only"}`, which
stores a payload and is a different DB state.

**`/blocked` is one of the enumerated exceptions**: this is the *session-close*
protocol, so by definition this session is NOT going to finish the follow-up, and
the blocker would otherwise become the silent stall this procedure exists to
prevent. So here a continuation is the norm — for a blocker whose resume is a
known action, pass one so coord can pick it up on clearance instead of just
recording it:

- `continuation` (preferred, typed) — e.g.
  `{"action":"run_skill","skill":"implement-phase","args":["<plan-stem>","Phase N"]}`.
  `args` MUST be a JSON **array**. A typed `{"action":"notify_only"}` is coord's
  explicit no-op action — it is NOT the default (the default is no continuation
  field at all).
- `continuation_prompt` (legacy free text) — the prompt coord injects into the
  resumed session when the gate clears (e.g.
  `run /implement-phase <plan-stem> "Phase N"`). Advisory context for the spawned
  agent — it is **not** what selects the action. Note it hardcodes `"repos": []`,
  which drops the spawned terminal's cwd onto the SHARED ROOT uncoordinated;
  prefer the typed `continuation`, or the HTTP `continuation_spawn` with `repos`
  populated.
- The continuation is resolved at **clearance** time, not registration time — do
  not assume this device will still exist; coord re-targets a healthy runner. It
  becomes *eligible* to spawn then — coord still requires a resolved ONLINE target
  device and a non-sensitive anchor, else it notifies instead.

**Still omit it when** the blocker is **sensitive** (security / credential /
billing / strategy) — those notify a human and never auto-spawn. **That is the
only omit case in `/blocked`.**

**Hard guard — if you are CLOSING, attach the continuation regardless of the
expected wait.** Charter rule 10's ≲2h monitor window is a reason **not to run
`/blocked` at all** (inside that window you are not blocked, you are waiting —
stay alive, monitor, and finish the item), **never** a reason to register a gate
and drop the dispatcher. A session closing for an unrelated reason (context
exhaustion, usage limit, an imminent crash) whose blocker happens to be a
20-minute CI wait must still attach: a watched gate with no dispatcher on a dead
session is precisely the silent stall this procedure exists to prevent. A
non-sensitive `operator_approval` gate IS unbounded in time and always takes a
continuation — but Step 5's plan-anchored HTTP fallback **403s**
`operator_approval` (`operator_approval_requires_operator_auth`), so that
exception is registerable only over MCP `coord_register_gate` or the
operator/acting `POST $COORD_HTTP_URL/coord/gates/register` route (Step 5 item 3).

**Know the delivery risk before relying on the spawn.** Continuations are
currently being **dispatched but never consumed**, and coord's 24h pending window
drops them permanently — treat the spawn as best-effort, and read the gate's
`continuation_consumed_outcome` rather than assuming a `consumed` continuation
ran (a **null** outcome means never claimed, which is worse than a recorded
`spawn_failed`). Attach it anyway: a best-effort dispatcher strictly beats none.

## Step 5 — Register (the call)

**Prefer the MCP tool `coord_register_gate`.** Pass the anchor (`work_unit_id` +
`phase_name`, or `claim_kind` + `resource_key`), the typed predicate object,
`clearance_audience`, and the optional continuation. Tenant derives server-side.
With `/coord/work-units/upsert` device-authed, the MCP path works from a device
session (the old `plan_ready`-needs-a-`plan_id` blocker is gone).

**HTTP fallback** when MCP is unavailable — for a **plan-anchored** gate it is now
TWO device-authed calls on coord's `require_jwt` sub-router (device/agent/service
JWT all work), first reachable transport wins:
1. **Upsert the work unit (always first):**
   `POST $COORD_HTTP_URL/coord/work-units/upsert {slug, title?, status?}` →
   **capture `work_unit_id`** from the response. `register-gate` does NOT upsert —
   it 404s `work_unit_not_found` if you skip this.
2. **Register:** `POST $COORD_HTTP_URL/coord/work-units/<slug>/register-gate` with
   body `{predicate, phase_name (required), continuation_spawn?,
   clearance_audience?, gate_class?}`. Reach it over the held device JWT; the runner's
   proxy-nonce write forwarder (`POST {runner}/coord-mcp/work-units/…`, header
   `X-Coord-Mcp-Proxy-Key` — or `Authorization: Bearer <nonce>` on newer configs —
   read from a live `.mcp.json`; the forwarder injects a
   fresh device JWT per request); or the acting-user-service token
   (`bash …/scripts/coord-acting-bearer.sh` — sourced from **`$COORD_AGENT_JWT`
   ONLY**: no `.mcp.json` carries a bearer anymore, every config is
   proxy-shaped) — all on the same two routes. It
   accepts any predicate-cleared kind; `operator_approval` is rejected **403** (a
   human decision, never a work queue).
3. **Claim-anchored gates** have no slug: register via MCP `coord_register_gate`
   (`claim_kind` + `resource_key`); the runner write forwarder
   `POST {runner}/coord-mcp/gates/register` (proxy-nonce authed, body
   `{claim_kind, resource_key, predicate, clearance_audience?,
   gate_class?, continuation_spawn?}` — needs the Phase-1a/1b deploys of
   `2026-07-21-gate-cascade-step3-proxy-rebase`; 404 = not deployed yet); or the
   operator/acting `POST $COORD_HTTP_URL/coord/gates/register` (default
   `https://coord.qontinui.io`) with the same anchor + predicate +
   `clearance_audience` + `gate_class?` + optional
   `continuation_prompt`.

A successful register returns **`201` with `{ "gate_id": "<uuid>" }`**.

### Warnings honesty — a `gate_id` is necessary, NOT sufficient

*(policy: `coordination` `gate-warnings-mean-not-usable`)*

The response also carries `initial_verdict`, `initial_verdict_reason`,
`warnings[]` and `steer`. **Branch on the VERDICT, never on
`warnings[].is_empty()`.** The gate is **REGISTERED-BUT-NOT-USABLE** when
`initial_verdict_reason` says the predicate **cannot be evaluated**, or when
`initial_verdict` is a terminal state it can never clear from (`misconfigured` /
`failed`). **This matters most here of all:** `/blocked` is the session-close
door, so a gate that silently cannot clear is a blocker nobody ever comes back
to.

**A non-empty `warnings[]` is NOT that signal — read it, do not count it.** Most
warnings are informational: every `pr_merged` gate on a coord-orchestrated repo
carries one, and `continuation_dropped_born_cleared:` drops only the
continuation while leaving a healthy gate. Branching on emptiness withdraws good
gates on routine runs — which at the session-close door means throwing away the
one watcher the blocker had.

When the verdict test DOES fire: re-check with
`coord_check_gate_predicate {predicate}` **against a control whose
answer you already know** (identical output on the control proves the predicate
is dead, not your anchor), re-register on a predicate coord can evaluate,
withdraw the unusable one, and close out quoting the NEW `gate_id`. Canonical:
`_gate-registration` → "Registration warnings".

### Masked-tool honesty

Per-agent MCP allow-set curation can **mask `coord_register_gate` as an unknown
tool** (coord `mcp/mod.rs` masking — an allow-set omitting the tool makes it read
as "no such tool"). If the call fails as unknown / method-not-found:

- Report exactly: **"gate NOT registered — coord_register_gate not in this
  session's tool allow-set"**, then
- Fall back to the HTTP route (Step 5: device-authed `POST /coord/work-units/upsert`
  then `POST /coord/work-units/<slug>/register-gate` for a plan-anchored agent
  session, else `POST /coord/gates/register` for a claim-anchored gate), OR surface
  the blocker to the operator if HTTP is also unavailable.

**NEVER report a gate as registered without a returned `gate_id`.** A silent "no
such tool" — or any response missing the `gate_id` — must never read as success.

**And the other mask:** a call that returns **`"Command failed with no output"`**
is a *dead cached transport*, not a masked tool — the tool is present, so the
fallback above never triggers. Presume the registration **LOST** (8 of 8 such
writes were adjudicated lost 2026-07-26, four of them `coord_register_gate`),
run **`/coord-revive`** for a typed verdict naming the live door, re-issue there,
and verify by read. This matters most at `/blocked`: a session closing on a gate
it wrongly believes it set leaves the blocker unwatched with nobody left to
notice. Canonical: `_gate-registration` → "Dead-transport honesty".

## Step 6 — Report

In your session-close report, list for each blocker either:

- **registered** — the predicate kind, the human-readable condition, the
  `clearance_audience`, and the **returned `gate_id`**; or
- **not a gate** — the blocker has no observable trigger; left in the report as a
  plain blocker; or
- **new predicate kind needed** — a real observable trigger exists but no current
  kind expresses it; interim `operator_approval` registered (with its `gate_id`),
  plus the proposed `<shape>`.

(Optional convenience: you MAY mirror what you registered into a `## Gates` block
in the plan file for the operator's eyeballs — but **coord is the source of
truth**; never require the block, never read it back as authoritative.)

---

*(Canonical registration spec: `_gate-registration` — keep this procedure and its
predicate-choice table in sync with that file and the consumer-skill copies.)*
