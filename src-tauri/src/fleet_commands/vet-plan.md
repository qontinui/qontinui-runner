# Vet Plan

Read a plan, research the codebase to validate its claims, and edit the plan in place to fix anything wrong, missing, or out-of-date. The plan is the deliverable — leave it stronger than you found it.

## Decision priorities

When the plan's choices need to be evaluated — pattern selection, abstraction boundaries, scope, sequencing, migration approach — judge them against the project's priorities, in this order:

1. **Powerful features** — does the design unlock real capability, or settle for a thin shim?
2. **Scalability** — does it hold up as data, concurrency, or surface area grows?
3. **Robustness** — does it fail safely, handle edge cases, and stay correct under adversarial conditions?
4. **Clean code** — is the structure clear, small, and consistent with how the codebase already solves similar problems?

These four are the **engineering priorities** — they decide *what* gets built. Two sibling sets bind alongside them:

- **UX priorities** (ordered: predictability → discoverability without clutter → no-surprise reversibility → honesty about uncertainty) — gates for any user-facing surface: an option that fails one is rejected even if it wins on power. Engineering breaks ties within UX. (Memory: `ux-priorities-alongside-engineering`.)
- **Implementation priorities** (ordered: verified throughput → early risk retirement → autonomy with checks → momentum through re-planning) — govern *how and when* work executes, orthogonal to the other two sets. (Memory: `implementation-priorities`.) For vetting they bind in three places: **sequence phases most-falsifiable-first** (assumption-killing probes before the builds that depend on them); **size the plan against `/implement-plan` coordinator orchestration** — a plan too large even for subagent-delegated execution must be split or explicitly flagged for a fresh-session handoff; and the **escalation rule** in the Decision policy below.

**Programming effort and backward compatibility are NOT factors.** Do not preserve a worse design because rewriting it would be more work, and do not flag a defect just because the fix is invasive. Do not endorse an awkward shape because it avoids breaking existing callers — breaking changes are expected in this project. If a plan's design is justified primarily by "less work" or "doesn't break X," that is itself a defect worth flagging.

## Arguments

- `$ARGUMENTS` — Path to the plan file (absolute or relative). If omitted, look for the most recently modified `*.md` under `$QONTINUI_PLANS_DIR` (see below) and the working tree root, and ask the user to confirm before editing.

## Plan directories

Plan paths resolve from two environment variables. The qontinui runner injects them
into agent sessions from its `paths.plans_dir` / `paths.plans_archive_dir` settings;
a session launched outside the runner will not have them.

<!-- plan-corpus:start -->
> **The DB is authoritative for reads; this directory is an AUTHORING surface**
> *(plan `2026-08-16-plan-corpus-authority-and-run-provenance`, D2/D3 — canonical
> statement in `CLAUDE.md` -> "Plan corpus authority").* Discovery, search and
> selection resolve against `agent.work_artifacts` behind qontinui-web; the
> shipped runner scanner flows filesystem edits INTO it (the half that writes
> *this* layer is opt-in — see the population caveat below). So:
>
> * **`$QONTINUI_PLANS_DIR` being unset is NOT an error and NOT a dead end.** It
>   is a supported configuration — a tenant may author entirely through the web
>   UI and own no plans directory at all. Resolve the plan from the corpus
>   instead of asking the operator to invent a path.
> * **`qontinui-dev-notes` is this fleet's OPTIONAL export target**, never a
>   requirement. No tenant needs a git repo to author, vet or ship a plan.
> * **A corpus that ANSWERS is not a corpus that is POPULATED.** The scanner
>   flows filesystem edits into the operational layer (`coord.work_units`)
>   whenever a plans dir and a coord base resolve, but the **body sync** that
>   fills the document layer (`body_push.rs` -> `agent.work_artifacts`) is
>   **opt-in** — built only under `QONTINUI_PLAN_LIBRARY_SYNC=1`, and gated
>   again per cycle on the tenant's `plan_capture` dial. **Either missing is a
>   silent no-op**, so a `200` carrying an empty list is **UNKNOWN, not "no
>   such plan"**: treat any zero-result corpus read as UNKNOWN unless you have
>   positively confirmed the body sync is on for this device.
> * **Do not probe by stem with `q`.** `GET /api/v1/plan-library?q=` matches
>   **title and body, NOT the slug**, so a by-stem `q` probe returns a false
>   negative for a plan that IS present. The exact door is
>   `?kind=plan&work_unit_slug=<stem>`; failing that, page `?kind=plan&limit=200`
>   and match `slug` yourself.
> * **When qontinui-web is unreachable**, read the local degraded-mode cache:
>   `$QONTINUI_PLAN_CACHE_DIR` (default `C:/claude/plan-corpus-cache/`) —
>   `PLANS-CACHE.md` for the index, `bodies/<kind>__<slug>.md` for bodies.
>   Refresh with `qontinui-claude-config/scripts/render-plan-cache.ps1
>   -MaxAgeHours 0`. **Say plainly that you are reading a cache and quote its
>   Rendered stamp**, and treat a stale or absent cache as **UNKNOWN, never
>   empty** — "this render did not see it" is not "it does not exist".
<!-- plan-corpus:end -->

- **`$QONTINUI_PLANS_DIR`** — the directory plans live in. **If it is unset, ask the
  user once where plans live, or fall back to `<workspace-root>/plans`** (a `plans/`
  directory beside the repos this session is working in). Never assume an absolute
  path from another machine.
- **`$QONTINUI_PLANS_ARCHIVE_DIR`** — optional, normally unset. When set and different
  from `$QONTINUI_PLANS_DIR`, it holds plans that have already been archived; look
  there only when resolving a stem that is not in the active directory.
- **Suite directories** — a multi-plan suite lives in its own directory *beside*
  `$QONTINUI_PLANS_DIR` (`$QONTINUI_PLANS_DIR/../<plan-dir>/`).

Neither directory has to be inside a git repo. Where this skill commits a plan edit it
first checks `git -C "<dir>" rev-parse --is-inside-work-tree`; if that fails, the edit
on disk is the whole ritual.

**Precondition — if the plan you are about to vet is untracked, commit and push it
first**, stamped `DRAFT`, from a worktree (never the primary/shared checkout); then
vet. This is mechanical, not hygiene: an untracked plan is invisible to coord's
`conflict_check`, unreadable by any peer, and outside the durable record. Plans are
supposed to be authored at `DRAFT` and committed at creation; committing one here is
repairing that, not replacing it.

Committing it does **not**, however, make the coord work unit attestable. `vetted`
is an *attested* registry status whose attester must differ from the unit's recorded
owner, and the comparison is on the actor key `device:<uuid>` — which carries no
session id, so every device-JWT session on this machine is ONE actor. A peer holding
a genuine agent JWT (`device:<d>:agent:<a>`) is a distinct key and does qualify; on a
device-JWT-only fleet there is no such peer. Publishing the plan buys reviewability,
not a qualifying attester. §5.4 covers what to write instead.

## Decision policy (binding)

When vetting surfaces a question, a fork in the road, or an unresolved trade-off, **answer it yourself** using these priorities, in order:

1. **Powerful features** — does this unlock capability the alternative cannot?
2. **Scalability** — does this hold up across many runners, many users, many specs, many tenants?
3. **Robustness** — fewer failure modes, clearer invariants, less silent drift?
4. **Clean code** — simpler model, fewer special cases, better separation of concerns?

**Explicitly NOT factors:**
- Programming effort, implementation time, lines of code, refactor cost
- Backward compatibility, migration burden, "would break existing callers"
- "More conservative" or "smaller blast radius" as a tiebreaker

If two options tie on the priorities above, prefer the one that **deletes more code** (delete over deprecate) and the one that **avoids new abstractions** when an existing one fits.

Do not escalate questions to the user. If the priority sets (engineering, plus the UX gates for user-facing surfaces) genuinely don't resolve a question, it is by definition not important enough for the operator to have an opinion on — pick the option you'd defend in review and justify it in the plan edit. If priority-unresolvable questions recur, note in the vet summary that the priority sets should be expanded. The only things worth surfacing to the operator are **operator-resource needs** (something only they can physically do) and a plan **too large even for coordinator-style orchestration** (per the implementation priorities).

When you resolve a question this way, write the decision into the plan with one sentence of rationale that names the priority that decided it (e.g. "**Resolved.** Use the registry-backed lookup — scales to N tenants without per-app caches.").

## Instructions

### 0. Publish activity to `coord.device_status`

Before reading the plan, UPSERT a status row so the operator dashboard
shows this session as vetting. This is the read-side of Phase 1.1 + 1.3
of plan `2026-05-21-coordination-improvements.md` — Phase 1.1 added the
`tenant_id` column on `coord.device_status`; Phase 1.3 wires the
dashboard's `MachineCard` to render it. This step fills the row.

The UPSERT is keyed on `device_id`, so each call overwrites the prior
row for this machine. Skip-and-warn if `device_id` cannot be resolved —
status publication is observability, not gating.

**Resolution chain:**

1. **`device_id`.** Env `QONTINUI_MACHINE_ID` first. Else read
   `~/.qontinui/machine.json` and parse the `"device_id"` field; fall
   back to `"machine_id"` if present (legacy shape). If neither
   source supplies a UUID, emit a single-line warning and skip the
   UPSERT (proceed to Step 1).
2. **`current_repo`.** The MAIN repo's directory name —
   `basename "$(dirname "$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null)")"`.
   NOT `basename` of `git rev-parse --show-toplevel`: from a linked git worktree
   that returns the WORKTREE's own directory name (`myrepo-wt-pr161-followup`,
   `myrepo-wt-lna`), so the dashboard tile groups this session
   under a repo that does not exist — and sessions run under
   `QONTINUI_AGENT_WORKTREE_MODE=1`, so that is the common path, not an edge case.
   `--git-common-dir` resolves to the main checkout's `.git` from a worktree and
   from the canonical checkout alike, and `--path-format=absolute` (git >= 2.31)
   keeps it from returning a relative `.git`. **If it prints nothing or `.`** —
   not a git tree — omit `current_repo` rather than sending what the expression
   then evaluates to (the parent directory's name, a wrong-but-plausible repo).
   (Inside a git submodule it yields `modules`; no submodules exist here.)
3. **`current_branch`.** `git symbolic-ref --short HEAD`.
4. **`tenant_id`.** Env `QONTINUI_TENANT_ID` if set; otherwise omit
   the field (coord column is nullable).
5. **Coord HTTP base.** Env `COORD_HTTP_URL` first, else
   `https://coord.qontinui.io`.

**`<plan-stem>`** is the plan filename without `.md` extension or
directory prefix (e.g. `2026-05-21-coordination-improvements`).

**Initial UPSERT** (fire before reading the plan):

```bash
curl -fsS -X POST "$COORD_HTTP_URL/coord/status" \
  -H "Content-Type: application/json" \
  -d "$(cat <<EOF
{
  "device_id": "<device_id>",
  "current_task": "vet-plan: <plan-stem>",
  "current_repo": "<repo basename>",
  "current_branch": "<branch name>",
  "details": {},
  "tenant_id": "<QONTINUI_TENANT_ID, omit field entirely if unset>"
}
EOF
)"
```

**Failure handling.** Any non-2xx response is logged as a single-line
warning (`⚠️ coord status publish failed: <status> <body>`) and the
skill continues. NEVER abort vetting on a status publication error.

**Clear on completion (Step 6).** After Step 5 stamps the plan as
VETTED, fire a clearing UPSERT with `current_task: null` so the
dashboard tile stops showing this vet session as in-flight:

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

If the skill aborts before Step 5 (e.g. the plan was already SHIPPED
and the user declined to overwrite), fire the clearing POST
best-effort. If that also fails, `prune_stale()` TTLs the row within
an hour.

### 0.2. Reserve the PLAN in coord (before the first edit)

**`/vet-plan` writes the plan file in place — Step 4's `Edit` calls are
mutations, and this command's own contract calls the plan "the deliverable."**
Until 2026-08-25 this command took **no** claim, reserve, or conflict check of
any kind: its only coord write was the Step 0 status UPSERT (observability,
explicitly non-gating) and the §5.4 work-unit upsert, which fires at the END,
after every edit has already landed on disk. Two sessions vetting the same plan,
or a vetter and an implementer on the same plan, shared **no key at all** and so
could never collide — the filesystem's last-writer-wins was the entire
mutual-exclusion mechanism. That is the hole this step closes.

This is the same reserve `/preflight` step 0 specifies
(`.claude/skills/preflight/SKILL.md` → "0. Reserve the plan (free today — do this
FIRST)"), on the same key, and `/implement-plan` Step 0.48 and `/vet-imp`
Step 1.1 issue the identical call. Wiring all three lifecycle commands to one
protocol makes **`/preflight` load-bearing for the entire plan lifecycle** — the
accepted trade: one implementation to keep correct beats four that drift.

**Granularity.** The reserve key is the **plan**, not a phase:
`plan:<plan-stem>` means *"this document is mine to move."* `/implement-plan`
Step 0.6's `plan:<plan-stem>:phase:<n>` claim is a strictly nested second
granularity meaning *"this phase's agent is mine to spawn."* A vetter and an
implementer share no phase number, so only the plan key can ever see them
collide.

#### Resolution

1. **`<plan-stem>`** — the plan filename without `.md` or directory prefix; the
   same stem Step 0 computed. This is the canonical cross-agent key that
   `coord.plans`, `coord.sessions.plan_slug`, `unit_ready` gates, the
   `Plan: <stem>` PR marker and `Depends-On:` all use.
2. **Machine UUID** — the UUID Step 0 resolved (`QONTINUI_MACHINE_ID`, else
   `~/.qontinui/machine.json` `"device_id"`, else the legacy `"machine_id"`).
   Reuse it; do not re-resolve. **The wire field does not follow the local key:**
   `/claims/*` takes `machine_id`, `POST /coord/status` takes `device_id`.
3. **`AGENT_SESSION_ID`** — resolve once and reuse for acquire and release:

   ```bash
   AGENT_SESSION_ID="${QONTINUI_AGENT_SESSION_ID:-${CLAUDE_CODE_SESSION_ID:-}}"
   ```

   `CLAUDE_CODE_SESSION_ID` is the harness session UUID — per-session-unique. If
   both are empty (older Claude Code), omit the field; never send an empty
   string.
4. **Coord HTTP base** — `COORD_HTTP_URL`, else `https://coord.qontinui.io`
   (Step 0's base).

#### The reserve call

Preferred — over MCP, which also scans sibling open PRs for the same slot:

```
coord_reserve_resource(kind="plan", name="<plan-stem>")
```

Fallback that survives a dead MCP transport — and this fallback is the *point*,
not a courtesy. `POST /claims/acquire` is **unauthenticated** (verified
422-on-empty-body 2026-08-21 and again 2026-08-25: *"missing field `kind`"*, not
a 401), while `coord_reserve_resource` has **no HTTP route at all**:

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
    "skill": "vet-plan"
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
measured **0.33 s**. If it ever again takes tens of seconds, the cost has
regressed: report the measured number, do not quietly raise the floor.

**Send the owner token.** `/preflight`'s written HTTP fallback omits both
`machine_id` and `agent_session_id`; this call must carry them. Without
`<machine_id>:<agent_session_id>` a second session **on this same box** silently
takes over the first's reservation — the identical bug that plan
`2026-06-03-coord-session-scoped-claim-owner-plan` (SHIPPED 2026-06-03; coord
PR #271 makes `acquire` SET/compare the owner token and the heartbeat/release Lua
match on it, qontinui-claude-config PR #49 sends it) fixed for phase claims.

#### Branch on the result

- **`granted` / `claimed`** — a fresh reservation. **This run is the OWNER and
  therefore the releaser** (Step 5, after the VETTED stamp). Proceed to Step 0.9.
- **`renewed`**, or a **`held` whose `current_holder_session` equals your own
  `$AGENT_SESSION_ID`** — this session already holds the reserve. This is the
  normal outcome under `/vet-imp`, which reserved at its Step 1.1 and then
  invoked this skill inside the same harness session. **A re-reserve by the same
  owner token is a renewal, not a conflict** — proceed, and do **not** release at
  the end of this run; the acquirer releases.
- **`held` by a DIFFERENT owner — STOP.** Do not edit the plan and do not stamp
  it. Report the holder and surface to the operator via `AskUserQuestion`
  (header `Plan reserved`, options **Abort** / **Wait** — poll every 30 s, then
  re-acquire). When `current_holder` equals THIS machine and
  `current_holder_session` differs, say so explicitly rather than implying a
  different box:

  ```
  Another session on THIS machine (session <current_holder_session>) already
  holds plan:<plan-stem>.
  ```

  It is a STOP, not a warning. The reason the reserve sits ahead of every `Edit`
  is precisely that stopping here leaves no trail in the plan file.
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
  do and it names siblings, name them in the Step 6 report and read them before
  deciding the plan's claims are current. If an older coord build still answers
  `fork_risk` or a non-empty `forking_siblings`, read it as the advisory overlay
  it always was — name the siblings in the Step 6 report — never as a hard
  holder.
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
> not edit the plan. Report the transport failure verbatim, run `/coord-revive`,
> and re-issue over the door it reports LIVE. If no door is live, surface to the
> operator via `AskUserQuestion` (**Abort** / **Proceed uncoordinated**) —
> proceeding is a decision someone makes, never a default reached by falling
> through an undocumented branch.

**Skip-and-warn only when there is no machine UUID at all.** If neither
`QONTINUI_MACHINE_ID` nor `~/.qontinui/machine.json` supplies one, emit a
single-line warning (`⚠️ plan reserve skipped: no machine_id available —
vetting without reserve coordination`) and proceed. This is the *only* branch
that proceeds without a reserve, and the asymmetry is deliberate: a device with
no machine UUID **cannot participate in coordination at all** (permitting it is a
stated trade), whereas a registered device that merely cannot *reach* coord is a
full participant whose peers are invisible — the case where proceeding is most
dangerous. Same observable ("no reserve acquired"), **opposite** correct
response. Collapsing them is the `silent-empty-is-unknown` class (served policy
`verification-and-evidence`) applied to a mutex.

#### Is a plan reserve MANDATORY? Ask the grammar registry, not this file

`/implement-plan` Step 0.7.5 owns the rule: *a resource is mandatory-reserve iff
coord has a **registered `SemanticResource` grammar** for it AND it is NOT
land-time re-pointable*, with the grammar registry as the single source of truth
and an explicit prohibition on hand-maintained lists in the skills. A plan
document satisfies the second half — a plan is never land-time re-pointable.

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

### 0.25. Capture the status block and read delivery — BEFORE any edit

Two facts this run depends on are destroyed by its own later steps, so capture
them **here**, while the plan on disk is still untouched and nothing has been
written.

**1. The existing status block, verbatim** — its lifecycle token, its date, and
any session marker. §5's single-stamp invariant instructs you to DELETE every
status-adjacent blockquote before writing yours, so a rule that reads the marker
*after* that point cannot discriminate anything. Hold these three values.

**2. Coord's derived delivery for this plan.** The stamp is an authoring-surface
artifact that lags by construction; coord derives `work_unit.status = shipped`
from merged PR citations and refuses a hand-written `shipped`
(`422 status_is_derived`), so delivery is the one signal that cannot lag.

```
coord_work_unit_list_citations(<plan-stem>) -> .delivery
```

HTTP twin: `GET $COORD_HTTP_URL/coord/agent-work-units/<plan-stem>/citations`,
which **refuses** on an unreadable citation surface. Do **not** substitute
`GET $COORD_HTTP_URL/coord/agent-work-units/<plan-stem>` — that superset route
deliberately answers `200` with `citations: []` plus a `citations_error` /
`delivery_error` key and no `delivery`, so reading its status line instead of
those keys turns a degraded read into a confident "no citations".

**Evaluate the arms in this order — 4, 3, 2, 1, 5, then 6. The order is
load-bearing** (several responses match more than one row, and 1/5 are the
conclusive ones). **Arm 6 is the DEFAULT**: anything not positively matched by
4, 3, 2, 1 or 5 is arm 6, so no response can fall off the end of the table:

| # | Response | Reading | Do |
|---|---|---|---|
| 4 | the error `no work-unit with that slug` (the MCP tool appends ` in your tenant`; the HTTP twin's 404 body deliberately does not, so it cannot leak whether the slug belongs to another tenant — match on the short form) | The unit does not exist. **The COMMONEST case** — a first-time vet of a `DRAFT` plan has no work unit, because §5.4 upserts it at the *end* of this run. | **Proceed to vet, and SAY the read was not-found.** An absent unit and an uncited-but-present unit are different facts; do not fold one into the other. |
| 3 | a top-level `merged_degraded_reason` is present | **UNKNOWN, whatever `delivery` says.** The field sits BESIDE `delivery` and is present even when the verdict could not be derived at all; while it is set, every citation's `merged: false` is UNKNOWN rather than an observation. | Treat as arm 2. |
| 2 | `evidence_complete: false` — **regardless of `shipped`** | **UNKNOWN — never "undelivered".** `evidence_gaps` names each gap. | Fall through to the stamp arms and **say the read was inconclusive**, per `verification-and-evidence` `unknown-must-not-render-as-a-default`. |
| 1 | `shipped: true` ∧ `evidence_complete: true` | The plan is closed **in substance**. | **Do NOT vet** (see "Act here" below). Route to closeout. |
| 5 | `delivery` present ∧ no `merged_degraded_reason` ∧ `evidence_complete: true` ∧ `shipped: false` | A clean, complete observation of *not delivered* — including the zero-citation case. | Proceed to vet. |
| 6 | **DEFAULT — anything not positively matched above.** Any error other than arm 4's, any unparseable or non-2xx body, a `citations_error` / `delivery_error` key, an absent `delivery`, or the tool masked / absent / on a dead transport (`"Command failed with no output"`) | **UNKNOWN.** Includes coord's `citation surface unavailable for work-unit …` (whose own text says it is NOT "this unit has no citations") and its generic `citation list failed: …`. | Treat as arm 2. On a dead transport run **`/coord-revive`** and re-issue over the live door before concluding anything. |

Arm 5 is stated as a **positive predicate on purpose.** It was written as
"anything else" and that was a fail-open default: every shape in arm 6 — coord
down, transport dead, degraded HTTP 200 — landed on *"a clean, complete
observation of not delivered → proceed"*, which is the exact inversion of the
clause this section cites, applied to the highest-base-rate failure in the
fleet. If a response does not positively satisfy arm 5, it is not arm 5.

Arm 2 drops any condition on `shipped` for the same reason. `shipped` and
`evidence_complete` are derived independently (`delivery_view::derive_delivery_from`:
`shipped = inputs.delivered`, `evidence_complete = evidence_gaps.is_empty()`),
and the "merged predicate degraded" gap is **unit-independent** — it fires for
every unit during a pre-migration window. So `shipped: true ∧
evidence_complete: false` is reachable, and keying arm 2 on `shipped: false`
let it fall through to the permissive arm.

**Do NOT invent a "no citations ⇒ UNKNOWN" arm.** Coord already treats an empty
`citations` array as a complete observation
(`{shipped: false, evidence_complete: true, evidence_gaps: []}`), and
second-guessing it would send every unvetted plan down the UNKNOWN path. Where a
*missing citation marker* is the real cause, the remedy is the backfill door
(`coord_work_unit_add_citation`), not a further verdict.

#### Act on the STOP cases HERE, not at §5

**"Do NOT vet" is unreachable from §5** — §4 *Edit the plan in place* has already
rewritten the plan on disk by then, so a refusal written there refuses nothing.
If, on the values captured above:

- delivery resolves to **arm 1** (the work has landed), or
- the status block reads `IN PROGRESS` and its session marker is **not
  positively identifiable as your own current session id** — absent, foreign, or
  unattributable — and it resolves to **case 3 or the unidentified default** of
  §5's disposition table (a live peer, an unmarked stamp, or one you cannot
  positively attribute). **Run case 2's probes HERE to decide that** — they are
  the only thing separating "adopt" from "a peer is mid-flight", and every one of
  them is read-only: does the stamping session's transcript tail show it died;
  are its worktrees clean and 0 ahead of `origin/main`; do any PRs or branches
  exist for the plan. If all of them hold this is case 2 (**adopt**) and you
  proceed — do not STOP. If any fails, STOP.

  **An ABSENT marker cannot reach case 2 at all**, because case 2's probes are
  keyed on "the marker is a session id ≠ yours" and there is no session to probe.
  So an unmarked `IN PROGRESS` stamp is an unconditional STOP here, exactly as
  §5's default row says — and it must be caught HERE, since §5's refusal runs
  after §4 has already rewritten the plan.

- the captured status block reads `SHIPPED`, `SUPERSEDED` or `OBSOLETE`. The
  do-not-overwrite paragraph in §5 states this rule, but by its own argument a
  refusal written there refuses nothing — §4 has already rewritten the plan, so
  §5 would be asking the operator to confirm a rewrite that is on disk. Catch it
  here instead. Note delivery arm 1 does NOT cover this case: a plan whose file
  says `SHIPPED` while its work unit carries no citations (PRs landed without the
  `Plan:` marker — the same failure `coord_work_unit_add_citation` exists to
  repair) reads **arm 5** and would otherwise proceed to vet,

then **STOP NOW**: report it with the evidence and make **no edit**. Release the
plan reserve only if THIS run acquired it — skip the release when the reserve
came back `renewed` (under `/vet-imp` the orchestrator holds it), and skip it
entirely in a copy of this command that has no reserve step of its own. Nothing has been written yet, which is the whole reason this
step sits ahead of §4 rather than inside §5.

Carry both captured results forward. §5's "`IN PROGRESS` is CONDITIONALLY
overwritable" **consumes** them — it never re-reads a stamp it is about to
delete, and never re-issues the delivery read.

### 0.9. Name this session + terminal after the plan

Derive a human name from the plan filename and use it to title the session
and the terminal window. The rule: take the plan **stem** (filename without
`.md` or directory), strip a leading `YYYY-MM-DD-` date prefix, and strip a
trailing ` plan` / `-plan` / `_plan` word if present.
(e.g. `2026-05-21-coordination-improvements` → `coordination-improvements`;
`2026-06-02-fleet-auth-plan` → `fleet-auth`.)

Run this once, substituting `<plan-stem>` (the same stem Step 0 computed):

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
fails, skip it silently and continue — naming never blocks vetting.

### 1. Read the plan in full

Use `Read` on the path. Don't skim — note every concrete claim:
- File paths, function names, line numbers
- "There is no X" / "we'll need to add Y" assertions
- Architectural decisions presented as locked
- Estimates and phase breakdowns

A plan written by someone else (or by past-you) usually has 2–4 things that look right but aren't. Your job is to find them.

### 2. Verify every concrete claim

For each claim in the plan, validate against the current codebase. Run these in parallel where possible:
- **File / module exists** → `Glob` or `Read`
- **Function or symbol exists** → `Grep` for the exact name
- **"No prior art for this" assertions** → `Grep` aggressively for related patterns; the prior art is usually one rename away (e.g. plan says "wrappers need a proxy" — search for `*proxy*.rs` across the repo)
- **Architectural assumptions** → spot-check 1–2 representative files
- **Memory references** → cross-check against `MEMORY.md` entries (memories can be stale; verify before quoting)

When the plan proposes a NEW abstraction, always check whether an existing one already covers it. The single most common defect in plans is "let's add helper X" when helper X already exists under a different name.

If the plan touches any of the SDK files below, also verify the runner-side parallel implementations exist — a plan that only edits the SDK side without naming the runner-side wireups is incomplete and should be flagged as a defect.

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
> If the plan ALSO touches response shape, query-param parsing, or status-code mapping (not just route registration), flag the absence of a `qontinui-runner/scripts/contract-smoke.ps1` run as a vet defect — the manifest diff catches missing routes but not field-stripping, query drops, or status-code drift.

### 3. Identify defects

Categorize what you find:
- **Wrong** — claim contradicts the code (path, signature, behavior)
- **Redundant** — proposes new code that duplicates existing infrastructure
- **Missing** — overlooks a concrete consumer, edge case, or coupled subsystem
- **Misaligned** — proposes a pattern inconsistent with how the codebase already solves the same problem
- **Stale** — built on a prior assumption that has since changed

Don't flag style preferences or hypothetical concerns — only material defects.

### 4. Edit the plan in place

**These are this command's first writes.** Do not reach them unless step 0.2's
plan reserve returned `granted` / `claimed` / `renewed` (or took the documented
no-machine-UUID skip). A foreign `held`, or a reserve that could not be answered,
stops the run before any `Edit`.

Use `Edit` (not `Write`) to surgically fix the plan, preserving the author's voice and structure. Specifically:
- **Add a "Discovered prior art" section** near the top if the plan missed existing infrastructure. Include a small table with `Piece | Location | Notes`.
- **Rewrite speculative sections** ("search for X, it might be in Y") to point at the actual file:line.
- **Replace proposed abstractions** that duplicate existing ones — show the existing pattern as the template.
- **Update file inventories** to match what will actually be created/modified.
- **Resolve open questions** inline. Two ways a question gets resolved:
  - *Research answered it* — strike through and explain (`~~...~~ **Resolved.** ...`).
  - *No new evidence, but the question is about an engineering trade-off* — apply the **Decision policy** above. Pick the option that wins on power → scalability → robustness → clean code, and write the decision into the plan with one sentence of rationale naming the deciding priority. Do NOT punt the question back to the user just because it requires judgment.
- **Add concrete file:line citations** where the plan was vague.
- **Verify `Depends-On:` targets exist** — see [Depends-On verification](#depends-on-verification) below.
- **Don't gut sections that are already correct** — the goal is targeted fixes, not a rewrite.

If the plan's overall direction is sound but an entire phase is wrong, rewrite that phase. If the overall direction is wrong, stop and surface that to the user before editing — don't silently rewrite the architectural premise.

#### Depends-On verification

A plan MAY declare upstream dependencies inline in its status blockquote
using a `Depends-On:` suffix:

```markdown
> **Status: VETTED 2026-05-21.** <summary>. Depends-On: 2026-05-20-default-tenant-propagation, 2026-05-19-some-other-plan.
```

Parser rule (same as `/verify-plan-status` and `/implement-plan` — keep
these three skills aligned):

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

For each dep stem, resolve to a plan file using the lookup chain:

1. Try `$QONTINUI_PLANS_DIR/<stem>.md`.
2. If that doesn't exist, check the suite dirs beside it (`$QONTINUI_PLANS_DIR/../<plan-dir>/`).
3. If `$QONTINUI_PLANS_ARCHIVE_DIR` is set and differs from `$QONTINUI_PLANS_DIR`,
   also try `$QONTINUI_PLANS_ARCHIVE_DIR/<stem>.md`.
4. If still unresolved, the dep is **missing** — flag it as a vet defect.

Use `Read` (a failure is the not-found signal) or `Glob` to check. A missing dep is a `Wrong` or `Stale` defect per Step 3's
categories — it means the plan references upstream work that either was
renamed, never written, or already archived under a different stem.

When you find missing-dep defects:

- Add them to the report under "Missing dependencies" with the exact
  unresolved stem.
- Include them in the auto-fix count if you correct the stem in the plan
  (the plan author may have typo'd the stem). If you can't determine the
  correct stem, leave the typo'd value in the plan but flag it for the
  user — do NOT silently delete the Depends-On entry.
- A plan with one or more missing deps is still vettable; the missing-dep
  finding does not block stamping `VETTED`. It just gets surfaced in the
  report.

A plan with no `Depends-On:` field is the common case — no verification
needed.

### 5. Stamp the plan as VETTED

After surgical fixes are written, add a status block immediately below the
H1 title. Use the same format `/verify-plan-status` and `/implement-plan`
use, so the lifecycle reads cleanly:

```markdown
# <Plan Title>

> **Status: VETTED <YYYY-MM-DD>.** <one-line summary of what was checked
> and what survived the audit>. Defects found: <count>. Auto-fixed: <count>.
> Surfaced for user: <count>. <Optional: pointer to follow-up plan if you
> created one in the report.>
```

#### Single-stamp invariant — read before stamping

A plan must have **exactly one** `> **Status:` blockquote between the H1
and the body. Before writing your stamp:

1. Read the top of the plan. Identify EVERY top-of-file blockquote that
   asserts a status, lifecycle state, or verification date — lines
   starting with `> **Status:`, `> **Edit YYYY-MM-DD —`, or `> **Update:`
   all count. Don't be picky about format; if it reads like a status
   declaration, it counts.
2. Use `Edit` to **delete every existing status-adjacent blockquote** —
   even if it came from a different skill (`/verify-plan-status` writes
   `NOT STARTED` / `PARTIAL`; `/implement-plan` writes `IN PROGRESS` /
   `SHIPPED`; older `/vet-plan` runs may have left `Status: VETTED ...`).
   Yours replaces all of them.
3. Then `Edit` again to insert your single new `> **Status: VETTED …`
   block.
4. If folding in history is useful (e.g., the plan was previously
   `NOT STARTED` per a verify pass, or had earlier verifier findings
   in a separate `> **Edit:` block), include the salient info in
   **one trailing line inside your new block**, prefixed `History:` or
   `Previously:`. Never as a sibling blockquote.

When `/vet-plan` and `/verify-plan-status` disagree on what the status
should be (the plan is both VETTED *and* NOT STARTED — a vetted plan
that nobody has implemented yet), consolidate: write the lifecycle
stage in the heading (`Status: VETTED <date> — implementation not
started.`) and put the verifier's findings in the body. The two
skills do NOT both stamp side-by-side.

#### Lifecycle states

| State | Owner skill | Means |
|---|---|---|
| DRAFT | author | plan written, not yet vetted |
| VETTED | `/vet-plan` | claims audited; ready to implement |
| IN PROGRESS | `/implement-plan` | implementation underway |
| SHIPPED | `/implement-plan` or `/verify-plan-status` | all phases live in code |
| PARTIAL | `/verify-plan-status` | some phases live, some open |
| NOT STARTED | `/verify-plan-status` | no implementation evidence |
| SUPERSEDED / OBSOLETE | any | terminal states |

`/vet-plan` writes only `VETTED`. If the existing block is `SHIPPED`,
`SUPERSEDED`, or `OBSOLETE`, do NOT overwrite — surface to the user
that they're vetting a closed plan and confirm they actually want this
rewrite. (`PARTIAL` and `NOT STARTED` are fine to overwrite — your vet
pass produces fresher information.)

##### `IN PROGRESS` is CONDITIONALLY overwritable — consumes Step 0.25

`IN PROGRESS` is deliberately **not** a fourth unconditional token in the list
above, and adding it as one would be a regression: it has three dispositions,
and the file stamp cannot tell them apart in either direction.

**This section consumes a CAPTURE STEP's two values** — the verbatim status
block (token, date, session marker) and the delivery arm from the arm table.
Which step captured them, and whether the stamp is still readable here, depends
on the command you are reading:

| Command | Capture step | Stamp still readable at this point? |
|---|---|---|
| `/vet-plan` | **Step 0.25** | **No.** §4 has already rewritten the plan, and the single-stamp invariant below deletes every status-adjacent blockquote, so the marker is gone. Consume the capture: do not re-read the stamp, and do not re-issue the delivery read. |
| `/implement-plan` | **Step 0.45 check 1** (read-only, pre-reserve) | **Yes.** Step 0.5 is that command's FIRST write, so the stamp is intact — read it there and run the arm table inline. |

If the capture already resolved to delivery arm 1, or to case 3 / the
unidentified default below, that run should have stopped at its capture step
without editing anything.

**The arms below are scoped to a status block that reads `IN PROGRESS`.** A
`DRAFT`, absent, `PARTIAL` or `NOT STARTED` block is governed by the paragraph
above this section, not by this table — do not read the "anything else" row as
refusing a first-time vet.

| # | Case | Discriminator (from Step 0.25's capture) | Disposition |
|---|---|---|---|
| 0 | The marker **IS** your own current session id | a resume, or a Step 0.5 re-run | **Refresh**, never take over: update the date and keep the trail. |
| 1 | Work has **landed** | delivery **arm 1** | **Refuse.** Do not overwrite; route to closeout. |
| 2 | The stamping session is **dead with zero work products** | the marker is a session id ≠ yours, AND its transcript tail shows death, AND its worktrees are clean and 0 ahead of `origin/main`, AND no PRs and no branches exist for the plan | **Adopt.** Keep the fresh `VETTED` and record the takeover as the `History:` line **inside that one block** (never a second blockquote — see the single-stamp invariant), naming both session ids and the evidence. |
| 3 | The stamping session is a **LIVE PEER** | the marker is a session id ≠ yours and case 2's checks do NOT all hold | **STOP.** Do not vet and do not implement. |
| — | **Anything else** — the stamp carries **no** session marker, or one you cannot positively attribute | no evidence of death, and no evidence it is yours | **STOP**, exactly as case 3. |

Evaluate case 0 first — a run that positively identifies the marker as **its own
current session id** is refreshing its own stamp, not taking one over.

**The unidentified default is not a formality.** An unmarked `IN PROGRESS` stamp
(hand-written, operator-written, or predating the marker convention) matches
neither case 2 nor case 3, and without this row the reader falls back to the
pre-change behaviour — overwrite — which is the regression this section opens by
forbidding. It bites hardest in the motivating scenario: under delivery arm 2 or
3 (UNKNOWN) with an unmarked stamp, no other row fires at all.

**Case 3 is invisible to the delivery read, by construction** — a peer
mid-implementation has no merged PRs yet, so the unit returns
`{shipped: false, evidence_complete: true, evidence_gaps: []}`, a clean
non-degraded arm-5 reading. Only case 2's four checks separate case 2 from case
3, and **the default when they fail is STOP, never adopt.** Adoption is the
*earned* branch; stopping is the fallback.

**Routing to closeout (cases 1 and 2's terminal half) has two halves, and one of
them you may not be able to write.** `plan-discipline` "Closeout" assigns
closeout to the unit's **owner**, and the coord statuses are gated: `shipped` is
derived (`422 status_is_derived`) and `vetted` / `superseded` / `obsolete` are
attested (`403 self_attestation_forbidden` when you own the unit). So stamp the
**plan file** terminal status — always yours to write — and treat the coord
transition as best-effort, reporting it as owed if refused. `/implement-plan`
Step 6 draws the same distinction; do not report a closeout complete on the
strength of the file stamp alone.

Sources — read them before "simplifying" this condition away, because each
records a real cost already paid:
[[feedback_adopt_dead_session_in_progress_plans]] (case 2's checks; a plan
stamped by a session that died at its usage limit with zero work products should
be adopted, not re-vetted) and
[[feedback_plan_already_stamped_by_other_session_is_live_peer]] (case 3; on
2026-06-08 a session rationalized a peer's stamp as "an automated self-stamp",
built four phases plus tests plus PR #479, and found on rebase that peer PR #468
had merged the same plan one commit after its branch point — a full
implementation wasted).

This stamp is mandatory. A vetted plan without the stamp is
indistinguishable from a draft, and `/implement-plan` will treat it as
still-aspirational.

After stamping, fire the clearing `POST /coord/status` documented in
Step 0 with `current_task: null` so the dashboard tile stops showing
this vet session as in-flight.

**Then release the plan reserve — only if THIS run acquired it.** In the same
try/finally (so it fires on every abort path too, including the step 0.2 conflict
flow's **Abort**):

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
token is the match key, so omitting it (or sending a different session) returns
`"not_held"` and leaves the reservation to TTL out. **Skip the release entirely
if step 0.2 returned `renewed`**: under `/vet-imp` the orchestrator acquired the
reserve and `/implement-plan` still has to run on it, so releasing here would
unreserve the plan mid-lifecycle. `"not_held"` is otherwise fine and idempotent.

### 5.4. Register a `unit_ready` gate for the vetted plan (dispatchable-work queue)

*(canonical spec: `_gate-registration` — keep copies in sync)*

A VETTED plan is **ready, dispatchable work** — not a human decision. When you
stamp VETTED, register (or **refresh** an existing) `unit_ready` gate (coord
tracks the plan as a generic **work unit**) so coord holds the ready plan as
watched, dispatchable work instead of letting it rot until someone clicks — and,
when one of the exceptions below applies, register a SECOND gate beside it (step
5b) whose queued continuation dispatches into a session the operator can **see**
if nobody picks the plan up. This **replaces** the old
`operator_approval`-bootstrap gate that used to queue ready work: a work queue is
`unit_ready`, NOT `operator_approval` (`operator_approval` is for genuine human
decisions only — see the predicate guidance in `_gate-registration`).

> **DEFAULT — and on the `unit_ready` gate, UNCONDITIONAL: register it
> CONTINUATION-LESS — omit the
> continuation entirely: no `continuation` and no `continuation_prompt` (MCP
> `coord_register_gate`), and no `continuation_spawn` (HTTP `register-gate`).**
> All three spellings are the SAME knob (coord materializes both MCP fields into
> the DB's `continuation_spawn` and both spawn), and the default is **omission**
> — `continuation_spawn` NULL — *not* the typed `{"action":"notify_only"}`, which
> stores a payload and is a different DB state.
> The gate's durable record is always required — it is what keeps the ready plan
> visible instead of silently dropped. The *dispatching* continuation is
> the exception: charter rule 10 ("Finish to zero") makes a session finish its own
> follow-ups in-session, and when the follow-up already runs in THIS session a
> continuation makes coord ALSO queue a fresh runner-terminal session for
> it — a duplicate, parallel run of the same work (the exact concurrent-WIP clobber
> the coordination layer exists to prevent). So by default: still upsert the work
> unit, transition its status (per the registry step below — attempt `vetted`, fall
> back to `vetted_unattested`), and register the `unit_ready` gate for
> registry/dashboard visibility keyed on whichever status actually landed (it
> auto-clears by predicate), but with NO continuation of any kind.
>
> ⚠️ **On the `unit_ready` gate this is not a default — it is UNCONDITIONAL,
> `/vet-imp` included (corrected 2026-08-30).** The ordering this section mandates
> makes a `unit_ready` gate **born cleared**, and a continuation on a born-cleared
> gate is not a safety net at all. `ready_verdict` is a bare
> `status != ready_status` compare, step 4 transitions the unit BEFORE step 5
> registers the gate keyed on the status that landed — so `status == ready_status`
> by construction — and a freshly-upserted unit has no open siblings. `Cleared` is
> therefore the only reachable verdict from the first evaluation onward: the 10 s
> `run_gate_sweep` clears the gate and dispatches its continuation within ONE tick
> of registration. Measured 2026-08-26 (gate `87e8e72b`): dispatched, then
> `consumed_outcome: "spawned"` 509 ms later, while `/implement-plan`'s cancel
> arrived tens of minutes afterwards to a `409 already_consumed` — a redundant
> terminal on **every** completed `/vet-imp` run, not a residual case. coord now
> refuses that arming at the door (see the `continuation_dropped_born_cleared:`
> carve-out under step 7), so attaching one here is not merely pointless, it is
> dropped. The dispatching continuation goes on the SEPARATE net gate below.
>
> **EXCEPTION — `/vet-imp` registers a SECOND gate: the vet→implement safety net
> (inversion 2026-07-28; re-shaped 2026-08-30).** When this vet runs as the first
> half of the `/vet-imp` chain, register the continuation-less `unit_ready` record
> gate exactly as above, and then register **one more gate** — step 6 below — on
> the SAME `work_unit_id` under a **DISTINCT `phase_name`**
> (`"vet→implement safety net"`), carrying the dispatching `continuation_spawn`
> and this predicate:
>
> ```json
> {"kind": "time_elapsed", "duration_secs": 1800}
> ```
>
> Omit `since`: it defaults to registration time and coord self-containment-stamps
> it there, so the window is anchored to the moment of arming rather than to
> evaluation. The split is deliberate — do not "fix" it back into a continuation
> on `unit_ready`, and do not re-spell it `unit_status`.
>
> *Why the two gates are separate.* One gate was answering two different
> questions — *this plan is ready, dispatchable work* (a record, true the instant
> it is vetted) and *nobody has picked it up, so dispatch a session* (a wait,
> false the instant it is armed) — and could not satisfy either without breaking
> the other. Split, both hold: the record gate publishes immediately, the net
> stays genuinely unsatisfied for the whole window.
>
> *Why `time_elapsed` and NOT `unit_status: in_progress`.* `UnitStatusEvaluator`
> clears when the unit's status **equals** the predicate's value, and there is no
> negation anywhere in the predicate vocabulary. A net keyed on `in_progress`
> would therefore dispatch a fresh implementation session at exactly the moment
> implementation had already begun — the duplicate this split exists to remove,
> re-created with a longer fuse. `time_elapsed` needs nothing new, is a pure clock
> read, and states the real condition: *N seconds passed and nobody retired this
> net.*
>
> *Sizing the window.* **1800 s (30 min)** is the interim value. The floor is one
> 10 s `run_gate_sweep` tick, but the binding constraint is the real
> vet→IN PROGRESS latency — `/implement-plan` has to get through Step 0.45
> reconnaissance, the plan reserve and the first phase claim before it reaches its
> Step 0.5 stamp, which is minutes — and 1800 s is sized above that. Erring long
> costs latency on a genuinely stranded plan; erring short re-creates the
> every-run duplicate spawn with extra steps. Measure the latency across recent
> `/vet-imp` runs before changing the number.
>
> ⚠️ **The net gate BLOCKS the record gate until it is MUTED — cancelling its
> continuation is not enough.** `open_sibling_gates` counts every open gate on the
> same `work_unit_id` **across phase names**, excluding only `unit_ready`
> predicates on that unit and rows with `muted = true`. The net is a
> `time_elapsed` gate on this unit, so it is a **blocking sibling** of the
> `unit_ready` record gate: while it is open, `unit_ready` evaluates `Open` with
> the reason *"…but 1 sibling gate(s) still open"* and stops publishing "ready,
> dispatchable work" for the whole window. And `cancel_continuation` writes only
> the `continuation_*` columns — **the verdict is untouched** — so a chain that
> cancels and walks away leaves the net gate open FOREVER, pinning `unit_ready`
> open with it: the silent fail-open this section exists to prevent, merely
> relocated. `/implement-plan` Step 0.5 therefore **cancels and THEN mutes** the
> net (`coord_mute_gate` `{gate_id}`, or its twin
> `POST .../coord/gates/<id>/agent/mute`) — the cancel is the race-safe
> stamp, the mute is what removes the row from `open_siblings` and lets the record
> gate clear. Mute the NET gate only; never mute the `unit_ready` record gate,
> which must stay unmuted to clear at all.
>
> *Why arming the net is safe.* `/implement-plan` **retires it when it stamps IN
> PROGRESS** (its Step 0.5, via `coord_cancel_continuation` `{gate_id, reason}`
> or the equivalent REST twin
> `POST $COORD_HTTP_URL/coord/gates/<gate_id>/agent/continuation-cancel` — one
> capability, two agent-side transports, the same takeover mechanism its Step 0.6
> has always used — then the mute, `coord_mute_gate` `{gate_id}` or its
> `/agent/mute` twin). Because the net's predicate is
> genuinely unsatisfied for 30 minutes, that cancel lands **pre-dispatch**, which is the
> state Step 0.5 documents and handles. Pre-dispatch is a supported call, not a
> 404: `cancel_continuation` deliberately omits the
> `continuation_dispatched_at IS NOT NULL` guard — *"the pre-dispatch stamp is the
> whole point"* — so the stamp lands before there is anything to race.
>
> | window at the IN PROGRESS stamp | remedy from THIS session |
> |---|---|
> | pre-dispatch (`dispatched_at == null`) — **the expected state**, and the one the 30-minute net exists to produce | `POST .../coord/gates/<id>/agent/continuation-cancel` `{reason}` stamps it cancelled pre-dispatch, **then** the mute — `coord_mute_gate` `{gate_id}`, or its twin `POST .../coord/gates/<id>/agent/mute` — to unblock the record gate. `coord_withdraw_gate` also works and does both at once |
> | post-dispatch, unconsumed (a net older than its window, or one registered by an older coord) | same cancel → `200 {"cancelled":true}`, then the same mute |
> | already consumed | `409 already_consumed` — the spawn happened; report it, do not claim a clean takeover. **Still mute the gate**, or it pins `unit_ready` open |
>
> **This paragraph used to say the residual was unavoidable.** It said the cancel
> could not land from the session doing the work and that a redundant terminal
> "may still spawn" — because it named the OPERATOR route, which answers an agent
> `401 operator context missing; SSO required`. That belief cost a real spawn
> (gate `7902e457`, `continuation_consumed_outcome: "spawned"`, 2026-08-20,
> against a plan already stamped SHIPPED). Exercised end-to-end on the agent twin
> 2026-08-21, gate `336701f1`: dispatched 09:39:44Z, cancelled from the
> implementing session at its IN PROGRESS stamp, `200 {"cancelled":true}`.
>
> So the honest trade is now: a **silent** strand (old behaviour — plan rots
> unnoticed) has been exchanged for a net the completing chain retires itself,
> and — since the net is armed on a predicate that is false for 30 minutes rather
> than one that is already true — it no longer fires on chains that hold.
> Step 0.45 reconnaissance and the Step 0.6 claim conflict remain the second line
> of defence for a spawn that slips through — they are no longer the first.
>
> *Why the net is necessary at all.* The old continuation-less default disabled
> the safety net at exactly the moment it was needed. `/vet-imp` stalled after
> vetting repeatedly (diagnosed 2026-07-28, reproduced live): the plan got stamped
> VETTED, the gate cleared, and the chain ended without `/implement-plan` ever
> being invoked — leaving the plan **STRANDED**, vetted with nothing queued to
> pick it up. Standalone `/vet-plan` would have queued a dispatching
> continuation and self-healed; `/vet-imp` specifically opted out of it. The net
> restores that self-healing without paying for it on every completed run.
>
> Net effect: **a completed chain cancels AND mutes its own safety net; a dropped
> chain self-heals into a fresh, visible session 30 minutes later.**
>
> Follow step 2's `device_id` resolution and step 6's `continuation_spawn` shape
> below — including a populated `repos` — for the net gate. Standalone vetting
> uses the SAME net gate: nothing retires it there, so it fires at 30 minutes and
> dispatches the visible session that is the whole point of vetting standalone.
>
> **Attach a continuation only when the follow-up will outlive this session**
> — "finish to zero" is intent, and intent cannot survive exogenous session death:
> (1) the wait exceeds charter rule 10's monitor window (rule 10 keeps a session
> alive and monitoring only for "an observable signal and a short expected wait
> (≲2h: deploy, CI, merge train)"); (2) vetting **STANDALONE**, where no
> implementation follows this session and the dispatch into a visible session is
> the whole point — **or vetting under `/vet-imp`, per the exception above,
> where the continuation is the net under a chain that may drop and
> `/implement-plan` Step 0.5 cancels-then-mutes it when the chain holds**. In
> BOTH cases the continuation rides the step-6 `time_elapsed` net gate, never
> the `unit_ready` record gate;
> (3) `operator_approval` / genuine human-decision gates, which
> are unbounded in time (except sensitive work — security/credential/billing/
> strategy — which stays notify-only unconditionally); (4) cross-session
> dependency chains whose follow-up belongs to a different work unit or device.
> **(3) and (4) do not arise in §5.4 by construction** — this section registers a
> `unit_ready` record gate (and, at most, one `time_elapsed` net gate beside it) on
> THIS plan's own work unit, and `operator_approval` is both
> the wrong model here (a work queue is not a human decision) and 403-rejected on
> the device-authed `register-gate` door. In §5.4, decide between (1) and (2) only
> — and note that (2) now covers BOTH standalone vetting and `/vet-imp`, so in
> practice §5.4 registers a net gate in the common cases and skips it only for
> the narrower "this session is carrying the work to completion and the wait is
> inside rule 10's window" residue. What is being decided is whether the SECOND
> (net) gate exists at all; the `unit_ready` record gate is registered either way
> and is continuation-less either way.
> Sessions also die exogenously (usage limit, crash, reboot) — if you are
> *stopping* incomplete-because-WAITING, that is `/blocked`'s session-close
> protocol and it takes a continuation. (Canonical: `_gate-registration` →
> "Continuation policy".)
>
> **Verify the spawn path before relying on it.** `continuation_spawn` dispatch had
> a verified failure on this device (`spawn_failed: Failed to spawn shell:
> CreateProcessW ...` from a `%TEMP%\qontinui-identity-<uuid>\claude` shim path,
> 2026-07-16, both gates of the pr-failing-check-details plan). Not asserted fixed
> or still live — verify. If the spawn IS your recovery path, read the gate's
> `continuation_consumed_outcome` rather than assuming a `consumed` continuation ran.

Register exactly once per VETTED stamp (refresh, don't duplicate):

> **Agent sessions: use the device-authed work-unit door.** A vetting agent holds
> a coord **device JWT** (carrying `tenant_id`) but **no `OperatorContext`**. The
> work-unit **write** routes live on coord's `require_jwt` sub-router, so a
> device JWT resolves `tenant_id` server-side and CAN upsert + register — the old
> operator-only `/coord/plans/*` wall (which 403'd a vetting agent and silently
> broke §5.4 auto-dispatch) is gone. Registration is now **TWO calls** — upsert
> then register-gate — because `register-gate` does NOT upsert (it 404s
> `work_unit_not_found` if the slug is absent). Prefer, in order: (1) MCP
> `coord_register_gate` (now usable from a device session — it can upsert the work
> unit then register, both over the `/coord-mcp` proxy); (2) the **direct
> device-authed HTTP** routes when a raw device JWT is held:
> `POST $COORD_HTTP_URL/coord/work-units/upsert {slug,title?}` → capture
> `work_unit_id`, then
> `POST $COORD_HTTP_URL/coord/work-units/<plan stem>/register-gate`; (3) the
> acting-user-service token (`coord-acting-bearer.sh`) on the same two routes if no
> device identity is available. The legacy operator-only `/coord/plans/upsert` +
> `/coord/gates/register` are removed (coord P4) — do not use them.
> (`2026-06-15-coord-device-authed-plan-ready-gate-registration`, generalized to
> work units.)
>
> **Reads are on the `agent-` paths, not the write paths.**
> `GET /coord/work-units/<slug>` and `GET /coord/gates` are the operator
> dashboard's `TenantId`-tier routes and answer a device JWT with **403
> `tenant_not_resolved`**. The device-authed read doors are
> `GET $COORD_HTTP_URL/coord/agent-work-units/<slug>` (returns `{work_unit,
> recent_history, citations}`) and `GET $COORD_HTTP_URL/coord/agent-gates`
> (same query params as the operator gates list, incl. `work_unit_id` and
> `phase_name`). `/coord/agent-gates` is the **read** door; the gate **writes**
> have their own device-authed twins under an `/agent/` **INFIX** —
> `POST /coord/gates/:gate_id/agent/{reject,reopen,mute,unmute,snooze,
> continuation-cancel,force-clear,audience}`. Note the two spellings: `agent-`
> PREFIX for reads, `/agent/` INFIX for writes. `attest` **and `withdraw`** are
> device-authed on their BARE paths (no twin exists, and there is no operator
> withdraw route at all). `continuation-consumed` / `continuation-deferred` are
> unauthenticated — the runner's delivery-ack loop, out of your scope but not out
> of reach. **`approve` is the only genuinely operator-only gate verb.**

1. **Upsert the work unit and capture `work_unit_id`.**
   `POST $COORD_HTTP_URL/coord/work-units/upsert` with
   `{ "slug": "<plan stem>", "title": "<plan H1>" }` (idempotent on slug; the stem
   is the filename without `.md`/path, the same `<plan-stem>` Step 0 used) →
   returns `{ "work_unit_id": "<uuid>", … }`. This is the **mandatory FIRST call**:
   the device-authed `register-gate` endpoint in step 4 does NOT upsert (it 404s if
   the slug is absent). The captured `work_unit_id` UUID anchors the gate AND is
   what the `unit_ready` predicate carries.
2. **Only if an exception above applies** (otherwise skip — with no net gate
   there is no continuation to target): **resolve the operator's `device_id` DYNAMICALLY** (never hardcode a UUID):
   env `QONTINUI_MACHINE_ID` first, else read `~/.qontinui/machine.json` and parse
   `"device_id"` (fall back to `"machine_id"` if present). If neither yields a
   UUID, skip the net gate entirely — a `continuation_spawn` has no target
   without it — but still register the `unit_ready` record gate, and note the
   missing net in the report.
3. **Check for existing gates** anchored to this work unit
   (`GET $COORD_HTTP_URL/coord/agent-gates?work_unit_id=<id>` — find the OPEN
   `unit_ready` record gate for this plan, and any OPEN net gate beside it). If
   one exists, **refresh** it rather than creating a duplicate. **Before
   refreshing, retire the prior net gate** so the old queued runner-terminal spawn
   does not fire alongside the new one: for any row in that GET carrying a
   `continuation_spawn` that is not yet consumed or cancelled — **pre-dispatch
   rows included**, which under the 30-minute net is the usual state — fire
   `POST $COORD_HTTP_URL/coord/gates/:gate_id/agent/continuation-cancel`
   `{reason:"refreshed — superseded by re-registration"}`, **then**
   `POST $COORD_HTTP_URL/coord/gates/:gate_id/agent/mute` on the same gate. Both
   are the device-authed `/agent/` **infix** twins, so the device session does the
   whole loop itself: `/coord/agent-gates` discovers the row, the cancel retires
   the queued spawn and the mute stops the stale net blocking the fresh record
   gate as an open sibling. `cancelled_by` derives from the JWT and is not a body
   field. (The unprefixed routes are the operator's and answer an agent 401.)
   The cancel is **legal pre-dispatch** — `cancel_continuation` deliberately omits
   the `continuation_dispatched_at IS NOT NULL` guard — so do not skip it on a
   row that has not dispatched. Best-effort: a 409 `already_consumed` = a spawn
   already happened (report it, don't pretend the cancel landed; still mute).
   (canonical spec: `_gate-registration` → "Continuation cancel + refresh".)
4. **Set the work unit's registry status** — the prerequisite for step 5's
   `ready_status`. This is the "Set the work unit's registry status" block below
   (read current → attempt `vetted` → fall back to `vetted_unattested`); do it
   here, before registering, and carry the status that landed into step 5. It is
   written out after this list only because it is long.
5. **Register** via the transport cascade in the blockquote above (the work unit
   was already upserted in step 1, so `register-gate` will find it): prefer MCP
   `coord_register_gate`; when a raw device JWT is held, the direct device-authed
   `POST $COORD_HTTP_URL/coord/work-units/<plan stem>/register-gate` (resolves
   tenant from the device JWT; body `{predicate, phase_name (required),
   continuation_spawn?, clearance_audience?, gate_class?}` — `slug` comes from the path, the
   predicate carries the `work_unit_id` UUID from step 1). `register-gate` does NOT
   upsert — if step 1 was skipped it 404s `work_unit_not_found`. The acting-user
   token works on the same route when no device identity is held. The legacy
   operator-only `/coord/plans/upsert` + `/coord/gates/register` are removed (coord
   P4) — do not fall back to them.
   - **Predicate:** `{"kind": "unit_ready", "work_unit_id": "<uuid from step 1>", "ready_status": "<the status the registry step below ACTUALLY landed>"}`
     — coord auto-clears it when the work unit's `status` column reaches
     `ready_status` AND every other gate anchored to this unit is cleared. (The
     `register-gate` endpoint accepts any **predicate-cleared** kind; only
     `operator_approval` — a human decision — is rejected 403, so it can never
     become a work-queue-as-decision fallback.)

     > ⚠️ **Do the registry transition FIRST, then register the gate with the status
     > that landed.** `ready_status` is compared by exact `!=` against
     > `coord.work_units.status`, so a gate keyed on a status the unit will never
     > reach pins open forever — and it fails **OPEN**, not `failed`, so no
     > `gate_unclearable_terminal` alert fires and nothing surfaces it until the
     > ~7-day stale-open sweep.
     >
     > That is exactly what a hardcoded `ready_status: "vetted"` used to do here.
     > `vetted` is an **Attested** status: coord refuses it unless the attester's
     > actor key differs from the unit's recorded owner, and step 1's upsert makes
     > THIS session the owner (ownership is claimed by the first non-attesting
     > write — creation counts, even with no `status` field). So the session that
     > registers the gate was the one actor barred from satisfying it.
     >
     > Ordering the transition first removes the guesswork: `vetted` when the
     > attestation genuinely lands, `vetted_unattested` when it is refused. Never
     > `in_progress` — that is what `/implement-plan` Step 0.5 writes, and reusing
     > it makes "vetted, waiting to start" and "already being implemented"
     > indistinguishable.
   - **Anchor:** the plan (work unit). The `work_unit_id` comes from the step-1
     upsert; pass `phase_name` (the plan title, or a synthetic label like
     `"vet→implement handoff"` for a whole-plan gate — `phase_name` is **required**,
     since coord's anchor is `work_unit_id` + `phase_name` together).
   - **`continuation_spawn` — OMIT IT on this gate, ALWAYS.** The `unit_ready`
     record gate is continuation-less unconditionally: this whole field absent on
     the HTTP body, and no `continuation` / `continuation_prompt` on the MCP tool
     either — all three are the same knob, and this gate is born cleared, so coord
     drops any continuation armed on it (step 7's
     `continuation_dropped_born_cleared:` carve-out). When an exception DOES apply
     (most often: vetting standalone, **vetting under `/vet-imp`** — see the
     exception above — or a wait longer than rule 10's ≲2h window), the
     continuation goes on the SEPARATE net gate in step 6, targeting the
     operator's device with a **visible** session —
     ```json
     {
       "target_device_id": "<device_id from step 2>",
       "presentation": "terminal",
       "initial_prompt": "run /implement-plan <absolute path to the plan file>",
       "continuation_prompt": "run /implement-plan <absolute path to the plan file>",
       "repos": ["<the plan's repo slug(s) — see below>"]
     }
     ```
     `presentation: "terminal"` (coord PR #356) opens a VISIBLE terminal window on
     the target device running the `claude` CLI with the prompt as argv — the
     operator sees it and can interrupt (that is the point of a visible
     continuation, per the visible-gate-continuations plan).
     **Expand the plan path before you register it** — write the resolved absolute
     path, not the literal `$QONTINUI_PLANS_DIR`. The continuation runs later, in a
     shell on the target device, whose environment you cannot count on matching yours.
     **Populate `repos` with the plan's declared repo(s)** (from the plan's
     `**Repo(s):**` line, or the repos the plan touches) — do NOT leave it `[]`.
     An empty `repos` makes `acquire_continuation_workdir` skip worktree
     allocation and drop the continuation's terminal cwd onto the shared
     workspace root, uncoordinated — the exact concurrent-WIP clobber the
     coordination layer exists to prevent. With `repos` set, the runner
     provisions an isolated `.agent-worktrees/<id>/<repo>` worktree (the first
     repo) as the cwd, so the session edits a per-session worktree under a
     `kind=worktree` claim from the first tick. If the plan genuinely touches no
     repo (a pure-investigation plan), `[]` is acceptable.
   - **`clearance_audience`:** `unit_ready` auto-clears by predicate, so audience
     is moot for *clearing*; register consistently with the model (default
     `operator`) — the typed predicate, not a human, is what clears it.
   - **`gate_class`:** the body shape above accepts it. A routine `unit_ready`
     gate is `routine-review`, or omit it — omitting is safe and never a
     loophole. Full vocabulary and the "when to classify" rule: the `gate_class`
     bullet in the flagged-items section below, canonical
     `_gate-registration` → "`gate_class`".
6. **Register the SAFETY-NET gate — a SECOND gate, and only when a continuation
   exception applies** (`/vet-imp`, standalone vetting, or a wait longer than rule
   10's ≲2h window). Same door and same transport cascade as step 5, same work
   unit, **different `phase_name`**:
   - **Predicate:** `{"kind": "time_elapsed", "duration_secs": 1800}` — omit
     `since`; coord self-containment-stamps it at registration, so the 30-minute
     window is anchored to the moment of arming. Do not key this gate on
     `unit_status`: it clears on EQUALITY, so an `in_progress` net dispatches a
     fresh implementation session exactly when implementation has begun.
   - **Anchor:** the same `work_unit_id` from step 1, with
     `phase_name: "vet→implement safety net"`. It MUST differ from the record
     gate's `phase_name` — coord's anchor is `work_unit_id` + `phase_name`
     together, so a colliding pair refreshes the record gate instead of adding a
     second one, and you would end up back with one gate answering two questions.
   - **`continuation_spawn`:** the shape shown in step 5, with a populated
     `repos`. This is the **only** gate §5.4 registers that carries one.
   - **This gate is a blocking sibling of the record gate until it is muted or
     clears** — see the sibling warning in the exception blockquote above. Under
     `/vet-imp`, `/implement-plan` Step 0.5 cancels-then-mutes it at the IN
     PROGRESS stamp; standalone, nothing retires it and it fires at 30 minutes,
     which IS the dispatch. Say in your report that the record gate reads `Open`
     with a *"1 sibling gate(s) still open"* reason until then — that is the
     designed state, not a stalled gate.
   - Same honesty rules as step 7: no net gate reported registered without a
     returned `gate_id`, and a failure here is reported as **no net**, never
     folded into the record gate's success.
7. **Masked-tool honesty + verification:** if `coord_register_gate` reads as
   unknown/method-not-found (per-agent allow-set masking) fall back to the
   device-authed HTTP routes (upsert → register-gate), and NEVER report a gate
   registered/refreshed without a returned `gate_id`.
   - **Dead transport is a different failure:** `"Command failed with no output"`
     means the tool was present and the transport was dead, so the masking
     fallback above never fires. Presume the registration **LOST**, run
     **`/coord-revive`**, re-issue over the door it reports LIVE, and verify by
     read. Canonical: `_gate-registration` → "Dead-transport honesty".
   - **`initial_verdict` is ADVISORY — the row is born `open` regardless.**
     Registration never writes the `verdict` column (`GATE_INSERT_SHAPES`), so a
     response reading `initial_verdict: "cleared"` means the predicate was
     satisfied *at the door*, not that the gate is cleared. The sweep decides,
     and it re-evaluates against the siblings that exist by then — which is why
     the step-5 record gate really does end up `Open` behind the step-6 net, and
     why a gate satisfied at registration is **not** terminal. Report a gate's
     state from a ROW read (`coord_gate_inspect`), never from this response.
     Canonical: `_gate-registration` → "Registration warnings".
   - **A `gate_id` with a DEAD VERDICT is not a registered gate**
     [policy: `coordination` `gate-warnings-mean-not-usable`]. **Branch on the
     VERDICT, never on `warnings[].is_empty()`.** The row exists and the gate can
     never clear when `initial_verdict_reason` says the predicate **cannot be
     evaluated**, or when `initial_verdict` is a terminal state it can never clear
     from (`misconfigured` / `failed`). Then, and only then: re-check the
     predicate against a control, re-register on one coord can evaluate, withdraw
     the unusable one, and quote the NEW `gate_id`. Canonical:
     `_gate-registration` → "Registration warnings".
   - **A non-empty `warnings[]` is NOT that signal — read it, do not count it.**
     Two informational warnings are routine here. coord refuses to arm a
     continuation on a gate that is already `cleared` at registration: the
     registration still **succeeds**, a `gate_id` comes back and the row is a
     perfectly good record, but `continuation_spawn` is NULLed before the INSERT
     and the reason is pushed onto `warnings[]` and `steer` with the stable prefix
     **`continuation_dropped_born_cleared:`**. That gate clears fine — only its
     continuation was dropped — so KEEP the gate, quote its `gate_id`, say the
     continuation was dropped and why, and put the dispatching continuation on the
     step-6 net gate where it belongs. The other is coord's `pr_merged`
     orchestrated steer, which every `pr_merged` gate on a coord-orchestrated repo
     carries. Branching on emptiness would withdraw a healthy gate on every single
     `/vet-plan` run.

(If coord doesn't yet accept `unit_ready` — e.g. the deploy that ships the
work-unit surface hasn't landed — report the gate as NOT registered with the
reason, rather than silently registering an `operator_approval` fallback that would
re-create the work-queue-as-decision antipattern.)

**Set the work unit's registry status — attempt `vetted`, fall back on refusal.**
When you stamp VETTED in the plan file, transition the coord work-unit registry so
the `unit_ready` predicate above has something to match. Do this BEFORE registering
the gate, and key the gate on whichever status lands.

**Step A — know the current status first. It may already be past you.**

**You already have it: step 1's upsert returned it.** That upsert carries no
`status` field, so coord treats it as metadata-only and echoes the stored status
back as **`previous_status`** — alongside `new_status` and `transitioned`, the same
shape from `coord_work_unit_upsert` (MCP) and `POST /coord/work-units/upsert`
(HTTP) alike. Read it as:

- **`previous_status: null`, `transitioned: true`** — step 1 *created* the row.
  There is no prior status to protect: proceed to Step B. **The row is not
  status-less, though — a status-omitting INSERT seeds the empty string**
  (`VALUES (…, COALESCE($3::text, ''), …)`), so the value Step C's CAS guard
  needs here is `from_status: ""`, not `null` and not `"draft"`.
- **`previous_status: ""`, `transitioned: false`** — same empty seed, seen on a
  re-run: the row exists and was created by an earlier status-omitting upsert.
  Treat it exactly like the case above — nothing to protect, `from_status: ""`.
- **`previous_status: "<non-empty status>"`, `transitioned: false`** — the unit
  already existed at `<status>`. That is the value the rest of this step means by
  "the current status".

> **Do not truthiness-test `previous_status`.** `""` and `null` are different
> rows — one exists with an empty status, one did not exist a moment ago — and
> both are falsy. Branch on the explicit cases above; a `if (!previous_status)`
> collapses them and loses the CAS value for the commonest path of all, the one
> where this very session just created the unit.

Only if you did not keep the upsert response, re-read it with
`GET $COORD_HTTP_URL/coord/agent-work-units/<plan stem>` and take
**`.work_unit.status`** (that response is `{work_unit, recent_history, citations}`,
not a bare unit).

If the status is already `vetted`, `ready`, `in_progress` or `shipped`, **write
nothing and skip to the gate registration**, keying `ready_status` on what you just
read. §5.4 is a re-runnable refresh path, and a blind write here would DEMOTE a
unit a peer already attested — destroying the attestation and, with it, the `ready`
derivation this whole ordering exists to protect.

> ⚠️ **The re-read is `agent-work-units`, NOT `GET /coord/work-units/<plan stem>`.**
> The unprefixed read is the operator dashboard's `TenantId`-tier route and answers
> a device JWT **403 `tenant_not_resolved`**. The split is by VERB, not by prefix:
> the `POST` writes under `/coord/work-units/…` (`/upsert`, `/transition`,
> `/register-gate`, …) *are* device-authed, which is why Steps B and C below keep
> that path — it is only the `GET`s that moved to `agent-work-units`. Same
> operator-vs-`agent-` door split the transport blockquote above already calls
> out; it is restated here because getting it wrong is not cosmetic. A 403 yields
> no status, so the skip
> rule cannot fire AND Step C has no value for its `from_status` guard — **both**
> protections against demoting an attested unit fail from one wrong URL, and the
> failure reads as a credential problem rather than a wrong door.
>
> If the status is genuinely unavailable — you discarded the upsert response AND
> the re-read fails — that is **UNKNOWN, not `draft`**. Do not invent a
> `from_status`: a guessed one CAS-fails against every real row, and omitting it
> turns Step C into the unconditional write this ordering exists to prevent.
> Report the registry transition as **not made** and say why.
>
> **And do NOT register the `unit_ready` gate on a guessed `ready_status`.**
> `ready_verdict` is a bare `status != ready_status` string compare with no
> non-empty guard, so a guess fails in one of two ways and the quieter one is
> worse: guess `"vetted"` or `"draft"` and the gate pins **open** until the ~7-day
> sweep (bad, but visible as a stalled gate); guess `""` — the value a
> freshly-created row actually holds — and it compares EQUAL, so the gate **clears
> immediately** and publishes "ready, dispatchable work" for a unit that never
> reached any vetted-class status. A false green is strictly worse than the rot
> this section exists to prevent. Report the gate as **not registered**, with the
> reason, and let the next run key it on an observed status.
>
> (If the failure is total — no device JWT and no MCP — then step 1 never ran
> either, so there is no `work_unit_id` and no gate to register in the first
> place. The reachable version of this branch is the narrower one: the upsert
> succeeded, its response was discarded, and the re-read then failed.)

**Step B — attempt the real thing.**

```
POST $COORD_HTTP_URL/coord/work-units/<plan stem>/transition
     {to_status:"vetted", by_actor:"<this session>"}
```

Attempt this even though it usually fails, because when it DOES land it is
strictly better: `vetted` is the documented lifecycle status, and coord's derive
worker promotes `vetted` + all-gates-cleared → the derived status `ready`. No
agent-reachable route produces `ready` any other way (the operator-transition route
can set it directly, but that is not yours). It lands when the attester is
genuinely not the owner — a different device, or a peer holding a real agent JWT —
and it is also the path the graduated-self-attestation relaxation
(`crates/coord/src/policies/lifecycle_autonomy.rs`) would open if the fleet ever
arms it.

**Step C — on a 403, fall back and move on.**

```
POST $COORD_HTTP_URL/coord/work-units/<plan stem>/transition
     {to_status:"vetted_unattested", from_status:"<what Step A read>", by_actor:"<this session>"}
```

Send `from_status` as a CAS guard so a peer attestation that landed between your
read and this write cannot be clobbered. Use the value Step A established —
including the literal `""` when this session just created the row.

The CAS is checked BEFORE the attestation authz, so a stale guard answers **`409`**
`{"error":"from_status does not match current status","current_status":"<actual>","asserted_from_status":"<yours>"}`
— not one of the three `403`s below. Distinguish them: a `409` means a peer moved
the unit under you, and **the response body already carries `current_status`**, so
re-apply Step A's skip rule against that value directly rather than re-reading (the
re-read would route back through a door that may be exactly what failed). Never
retry by dropping `from_status`; that converts the guard into the blind write it
exists to prevent.

Three 403 codes are expected here and NONE is a failure of the vet:

- **`self_attestation_forbidden`** — `vetted` is an Attested status and coord
  enforces separation of duties: the attester's actor key must differ from the
  unit's recorded owner. Step 1's upsert made this session the owner, so this
  session is the one actor barred from writing it.
- **`owner_unresolved`** — the unit has no recorded owner at all (a row predating
  the ownership widening, or one created by a token carrying no device id). SoD
  cannot be evaluated, so it is refused rather than passed vacuously. An ordinary
  Free transition claims ownership and un-strands it.
- **`attester_unresolved`** — YOUR token derives no actor key, i.e. it carries a
  `tenant_id` but no `device_id`. This is the one you will hit on the acting-user
  service token (transport tier 3 above). Fall back the same way, but say in your
  report that the attestation was not merely refused — it was **unattemptable from
  this door**, and would need a device- or agent-identified caller even to be
  evaluated.

Graduation relaxes exactly ONE of the three: `self_attestation_forbidden`. Both
`owner_unresolved` and `attester_unresolved` are returned verbatim and are **not**
relaxable — graduation is earned by a concrete actor's track record, so with no
resolvable actor key there is nothing that could have been earned
(`crates/coord/src/policies/lifecycle_autonomy.rs`, the
`OwnerUnresolved | AttesterUnresolved` arm).

None of the three is a transport problem, a credential problem, or a coord bug — do
not run `/coord-revive`, do not retry on another door, and do not report the vet as
failed. Two facts so you do not burn a cycle looking for a way around it:

- **The actor key is `device:<uuid>`** (or `device:<uuid>:agent:<uuid>` for an
  agent JWT), and it carries **no session id**. So a *different session on this
  machine* and a *subagent you spawn* share your actor key and are refused
  identically. A qualifying attester means a **different device**, or a peer
  holding a genuine agent JWT — note the same operator holding BOTH token shapes
  does satisfy the check, since `device:<d>:agent:<a>` and `device:<d>` are
  unequal strings.
- **The operator route** (`POST /coord/work-units/:slug/operator-transition`,
  admin/operator bearer) deliberately skips the SoD check. That is the operator's
  lever, not yours: routing your own vet through it would defeat the control it
  bypasses.

**What the fallback costs, stated plainly.** A unit left at `vetted_unattested`
never derives `ready`, because that derivation matches the literal `vetted`. You
keep dispatch (the `unit_ready` gate clears) and lose one derived observation.
`shipped` is unaffected — it derives from PR citations, independent of the
from-status. Treat the attestation as **owed, not blocked**: say in your report
that the unit sits at `vetted_unattested` and a `→ vetted` attestation is
outstanding. The plan is fully dispatchable meanwhile, and `/implement-plan` gates
on the plan file's VETTED stamp — it never reads the registry status.

The registry is directly writable — there is no longer a plan-ingest worker
mirroring the plan directory — so this explicit transition is what marks the unit
ready. The plan `.md` VETTED stamp + its commit/push remain the operator-private
artifact record. (A repo that is NOT coord sole-authority lands its PRs via normal GitHub flow.)

### 5.5. Offer to register a coord gate for a flagged-but-not-fixed item

*(canonical spec: `_gate-registration` — keep copies in sync)*

If vetting surfaces an item you flagged for the user that **cannot be resolved
now because it gates on an observable condition** (an upstream PR must merge, a
deploy/CI must go green, a metric must cross a threshold, a time window must
elapse, or an operator must approve), offer to register a coord gate for it — so
coord watches the condition rather than the finding rotting in the report. A
flagged item with no observable trigger (a product/scope call) is NOT a gate;
leave it in the report.

- **Default = explicit offer** via `AskUserQuestion` (header `Register gate?`,
  options Register / Skip), showing the derived anchor + predicate + condition.
  Under opt-in auto mode (env `QONTINUI_AUTO_GATE=1`) register without asking and
  note the gate_id in the report.
- **Anchor (zero user input):** `work_unit_id` (a UUID) from
  `POST $COORD_HTTP_URL/coord/work-units/upsert` with the plan stem as `slug`
  (capture the returned `work_unit_id`; or the device-authed
  `GET /coord/agent-work-units/<slug>` — the operator `GET /coord/work-units/<slug>`
  403s a device JWT);
  `phase_name` from the relevant phase/section heading. Anchor = (work_unit_id,
  phase_name).
- **Register:** prefer MCP `coord_register_gate` (kinds: `pr_merged`,
  `deploy_healthy`, `claim_terminal`, `operator_approval`, `ci_green`,
  `ref_exists`, `metric_threshold`, `time_elapsed`, `unit_ready`,
  `migration_at_head`, `infra_drift_clear`, `file_exists`, `sql_count`,
  `unit_status`, `gate_cleared`, `commit_live`; plus — **exception cases only,
  see the Continuation bullet below** — an optional typed `continuation` or legacy
  `continuation_prompt`). **HTTP fallback** when MCP is unavailable — for a
  plan-anchored gate it is now TWO device-authed calls on coord's `require_jwt`
  sub-router (device/agent/service JWT all work): (1)
  `POST $COORD_HTTP_URL/coord/work-units/upsert {slug, title?, status?}` →
  **capture `work_unit_id`**; (2)
  `POST $COORD_HTTP_URL/coord/work-units/<slug>/register-gate`
  `{predicate, phase_name (required), continuation_spawn?, clearance_audience?, gate_class?}` —
  `register-gate` does NOT upsert (404s `work_unit_not_found` if you skip step 1).
  Reach the two routes over a held device JWT, the `/coord-mcp` proxy (injects a
  device JWT), or the acting-user-service token; MCP `coord_register_gate` now works
  from a device session too. A claim-anchored gate (no slug) uses MCP or
  `POST $COORD_HTTP_URL/coord/gates/register` (default `https://coord.qontinui.io`).
  Tenant derives server-side — never pass it.
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
  registered-but-not-usable signal). Put the dispatch on a separate gate whose
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
  proven session identity. (Canonical: `_gate-registration` → "`gate_class`".)
- **Predicate choice:** wait-on-PR (non-coord repo) → `pr_merged`; work landing
  on a **coord-orchestrated repo** → `commit_live` `{repo, commit_sha}` with a
  **post-land main SHA** (NEVER a pre-land branch-head SHA — rebase-land rewrites
  SHAs so the gate rots open, gate `c14d103c` 2026-07-11; or anchor `unit_status`
  instead — **NOT `file_exists`, which is broken, see below**); wait-on-deploy →
  `deploy_healthy`; wait-on-CI → `ci_green`; burn-in → `time_elapsed`; metric →
  `metric_threshold` (explicit `labels` — e.g. `coord_ci_runner_count` MUST filter
  `{status:"idle"}`); a vetted plan that is ready, dispatchable work → `unit_ready`
  `{work_unit_id, ready_status}` — transition the unit FIRST and set `ready_status`
  to the status that actually landed (`vetted`, else the Free fallback
  `vetted_unattested`); a hardcoded Attested value on a unit you own never clears,
  since an owner may not attest (§5.4 has the full procedure; canonical:
  `_gate-registration`). (**NOT** `operator_approval` — `operator_approval`
  is for genuine human decisions, not a work queue); schema/alembic-at-head →
  `migration_at_head` `{schema}`; infra drift cleared → `infra_drift_clear`; a repo
  file/workflow existing → ⛔ `file_exists` is **KNOWN BROKEN (2026-08-05): it 403s
  fleet-wide on the contents API and the gate can never clear — use `commit_live`
  (post-land SHA) or `unit_status`**;
  a coord data count crossing a bound → `sql_count` `{query_id,op,n}` (whitelisted
  `query_id`, never raw SQL); an umbrella plan reaching a status → `unit_status`
  `{work_unit_id,status}`; another cross-anchor gate clearing → `gate_cleared`
  `{gate_id}`; needs-human → `operator_approval`. Anything
  **security / credential / billing / strategy-sensitive** → `operator_approval` +
  notify, never an auto-resuming gate, never silently auto-registered.
- **Masked-tool honesty:** if `coord_register_gate` fails as unknown/
  method-not-found (per-agent MCP allow-set masking, coord `mcp/mod.rs`), report
  **"gate NOT registered — coord_register_gate not in this session's tool
  allow-set"** and fall back to HTTP (or surface to the operator). NEVER report a
  gate registered without a returned `gate_id`.
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
  warnings. When the verdict test DOES fire, do NOT report the flagged item gated:
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
- The plan-file `## Gates` block is a **local convenience mirror only** — coord is
  the source of truth; never require it, never read it back as authoritative.
- **Re-registering for the same plan/anchor:** first cancel the prior gate's
  PENDING continuation (the §5.4 refresh-cancel rule applies to any
  continuation-carrying gate too) — `GET .../coord/agent-gates?work_unit_id=<id>` for rows
  carrying a `continuation_spawn` with `continuation_consumed_at == null ∧
  continuation_cancelled_at == null` (**including pre-dispatch rows** —
  `cancel_continuation` is deliberately unguarded on
  `continuation_dispatched_at`), then the cancel — `coord_cancel_continuation`
  `{gate_id, reason}` (native MCP) or
  `POST .../coord/gates/:gate_id/agent/continuation-cancel {reason}` (its REST
  twin); one capability, so use whichever transport is alive — followed by
  the mute — `coord_mute_gate` `{gate_id}` (native MCP) or its REST twin
  `POST .../coord/gates/:gate_id/agent/mute`. These are the device-authed
  doors, so a device session does the whole loop itself:
  `/coord/agent-gates` discovers the pending continuation, the cancel retires it
  and the mute stops the dead gate blocking its siblings. `cancelled_by` derives
  from the JWT and is not a body field. Best-effort. (This bullet used to say the cancel "stays operator-only" and that
  a device session had to reach an operator door; that was wrong — it named the
  unprefixed OPERATOR route, which answers an agent 401.) (canonical spec:
  `_gate-registration` → "Continuation cancel + refresh".)

**Attest-on-completion (close the loop).** Vetting normally registers gates
rather than completing gated work — but if this run also COMPLETES work that a
registered gate was watching, it MUST attest that gate (otherwise an agent-fact
gate rots open until a human clicks it).

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

### 6. Report

**First, decide WHO this report is for — it changes whether the turn ends.**
This skill has two callers, and only one of them wants a finished-looking
deliverable at this point:

| Invocation | What Step 6 does |
|---|---|
| **Standalone `/vet-plan`** | Emit the report and END THE TURN. Vetting is the whole job. |
| **First half of the `/vet-imp` chain** | **COLLECT** the report and **DO NOT END THE TURN.** Return control to the orchestrator, which carries it into `/vet-imp` Step 3. |

If you were invoked by `/vet-imp` (you will have been called via the Skill tool
from that orchestrator, with the plan path passed through), the chain is
**not finished** when you stamp VETTED — `/implement-plan` still has to run, in
this same session, on this same plan. So:

- **Do NOT write an end-of-turn summary.** Hand your report content back as an
  intermediate result; `/vet-imp` Step 5 emits the single combined report at the
  true end of the chain.
- **Do NOT write "proceeding to /implement-plan"** (or any equivalent) and stop.
  That sentence is the observed stall: it reads as a completed hand-off while
  nothing was actually invoked. See `/vet-imp`'s "Never narrate the hand-off"
  rule — the confirmation text and the `Skill: implement-plan` call belong in
  the SAME assistant turn, with the Skill call last.
- The VETTED stamp is a **midpoint**, not a finish line. Treat reaching it as
  the trigger to continue, not as a deliverable.

Brief — under 150 words. State:
- What the plan was about (one sentence)
- The 2–5 material defects found, each with the section they were in
- What you changed (referenced by section name, not full diff)
- The status stamp you added (`Status: VETTED <date>`)
- Open questions you **resolved using the Decision policy**, with the deciding priority in parentheses (e.g. "picked registry-backed lookup (scalability)")
- Anything you flagged for the user that you did NOT auto-fix — limit this to product/scope/stakeholder calls the Decision policy can't decide; engineering trade-offs should already be resolved in the plan

End-of-turn summary: one or two sentences — **when invoked standalone.**

**When invoked as the first half of `/vet-imp`: do NOT end the turn.** There is
no end-of-turn summary here, because this is not the end of the turn. Return
control to the `/vet-imp` orchestrator so it can run its Step 3 VETTED gate and
then invoke `Skill: implement-plan`. Ending the turn at this line is the
mechanical cause of the vet→implement stall diagnosed 2026-07-28: `/vet-plan`
was written as a standalone skill, and when it was composed into `/vet-imp` this
instruction fired and `/vet-imp`'s Steps 3-5 were never reached.

## Rules

- **Reserve before the first `Edit`.** Step 0.2's plan reserve is the exclusion
  primitive for this command; it runs ahead of every write so that a foreign
  `held` stops the run with the plan file untouched. A `held` held by a different
  owner is a STOP, and a reserve that coord could not answer is UNKNOWN — fail
  closed. The only branch that proceeds unreserved is a device with no machine
  UUID at all.
- **Edit the plan, don't rewrite it.** Surgical changes only. Preserve the author's structure unless an entire section is wrong.
- **Cite the code.** Every claim added to the plan should reference a file:line. "Almost certainly exists" is not good enough.
- **Verify memories before quoting them.** `MEMORY.md` entries are point-in-time observations; check the current code before treating a memory as fact.
- **Don't add new phases or scope.** If you find work the plan missed, note it in the report — adding a new phase is a decision for the user.
- **Parallelize research.** Use Glob, Grep, and Read in parallel; spawn `Explore` for broad surveys. When you spawn agents, never end a turn purely "awaiting notification" on their results — completion notifications are an optimization, never a guarantee (the wake-up channel is at-most-once); re-check for finished results on a bounded timer and proceed on evidence. On any nudge / system-reminder wake, FIRST re-check whether the awaited research already finished (evidence over memory), collect it, and continue — never re-spawn completed research.
- **No new files.** The plan file is the only thing you should be writing to.
- **Decide; don't escalate.** Engineering trade-offs surfaced during vetting must be resolved in the plan using the Decision policy. Effort and backward compatibility are not factors. Only kick a question up to the user when it's genuinely a product/scope/stakeholder call.
