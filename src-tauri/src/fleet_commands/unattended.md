---
description: "Session-close holistic audit — answers one question: if NO operator ever reads this session's output, will the implementation still be complete and correct? Classifies every unit of this session's work as LANDED / WATCHED / RECORDED / IMPEDED / DROPPED, converts every DROPPED item into a durable store, and where no store could hold it, sweeps for unactivated functionality and unimplemented plans before authoring a new one."
argument-hint: "[optional: area to focus on, or a plan slug this session was working]"
allowed-tools: Read, Write, Edit, Bash, PowerShell, Glob, Grep, ToolSearch, Agent, AskUserQuestion
---

# /unattended — will this survive an unread session?

**The predicate.** Assume the transcript of this session is never read by a
human. Assume this session's context is destroyed the moment it closes. Under
that assumption: **does the implementation still reach complete and correct?**

Everything below exists to make that question *answerable per item* rather than
answerable as a feeling. A closing session always *feels* done; the failure mode
this command exists to catch is the item that lives **only in the transcript**.

This is a **holistic audit of the automation loop**, not a code review. You are
measuring how well qontinui — coord, the gate registry, the memory store, the
plan corpus — actually captured the work, and you are repairing what it did not.

> **Autonomous by construction.** This command **acts, then reports** — it
> registers the gates, writes the memories, authors the plans. A command whose
> output an operator must read in order for the work to complete would itself be
> an instance of the defect it measures. Escalate only on the fleet's closed
> list (`escalation-bar` `escalation-closed-list`).

## Step 0 — Re-read served policy LIVE (do not skip, do not use memory)

Run **`/policy list`**, then fetch the documents this audit turns on:

`planning-and-scope` (`finish-to-zero`), `session-protocol`, `operating-rules`,
`verification-and-evidence`, `memory-and-notes`, `coordination`
(`gate-read-back`), `escalation-bar`, `git-operations`.

**Policy is re-read at CLOSEOUT, not only at session start.** Documents version
mid-session; a declaration made against v5 goes stale when v6 lands, and this
command's entire output is a set of declarations. Record the version of each
document you read and cite it in the report.

If the policy door is unreachable on every transport, say so explicitly and name
the failure you actually saw. **An unreadable policy door is UNKNOWN, not "no
policy"** — do not proceed as if unconstrained.

## Step 1 — Classify every unit of this session's work

Enumerate what this session actually did. Sources, in order of trustworthiness:

1. **Committed tree** — `git log` on every touched repo/worktree. Verify the
   **committed** tree, not the working tree; post-rebase and post-review edits
   sit uncommitted and vanish with the session.
2. **Open PRs** authored by this session (`/pr-status`, `mine=true` — fresh from
   coord's twin, never from memory).
3. **Registered coord gates** anchored to this session's work (`/gate-sweep`).
4. **The transcript itself** — the residue. Anything that appears *only* here is
   by definition a candidate DROPPED item.

Now put **every** unit into exactly one terminal state:

| State | Meaning | Evidence required |
|---|---|---|
| **LANDED** | On `main`, verified **by content** on `origin/main` | The landed SHA plus the content check. `gh` PR state is not evidence in EITHER direction — `closed, merged=false` and `MERGED` with `mergeCommit.oid == headRefOid` are **both** normal coord lands (`coord-ff-lands.md`). Ancestry only on the LANDED sha, never the head |
| **WATCHED** | Incomplete, but a coord gate watches the trigger and can resume it after every session dies | `gate_id`, **read back** after registration (`coordination` `gate-read-back`) |
| **RECORDED** | Not resumable work, but a durable record exists so the next session need not re-derive it | The memory record id / plan slug / policy clause |
| **IMPEDED** | Not done because a condition that is **currently true, environmental and shared** blocks it — and that condition is now posted where the next session will be handed it | The returned `finding_id`. Cite an `alert_key` too *if you actually have one* — `GET /coord/alerts` takes a device JWT, and coord#1601 added a machine arm over a closed allowlist (`FLEET_INFRA_MACHINE_KINDS`), but that arm is **legacy-posture only** and production runs `COORD_ALERTS_TENANT_STRICT=1`, so for exactly this condition class its silence is **still** never evidence — see the alerts note under 2b-findings |
| **DROPPED** | Exists only in this transcript | — |

IMPEDED is a *converted* state, not a softer DROPPED: it is earned by the
returned id, never by the blocker being real. An item whose post returned no
`finding_id` is **DROPPED**, exactly as a gate you did not read back is.

**Every DROPPED item is a defect in the automation loop.** Not necessarily a
defect in your judgement — the work may have been correctly deferred. The defect
is that the deferral was not durably captured. Steps 2 and 3 convert them.

### Step 1b — Finish-to-zero conformance

Independently of capture, audit this session against `planning-and-scope`
`finish-to-zero` and `operating-rules`:

- Was any task narrowed, deferred, or sampled **because it was large**? That is
  never a valid reason. Name it.
- Was a sweep reported as complete while its detector reached only part of the
  tree? Check the detector's **reach** before quoting any count — a pathspec like
  `'dir/**/*.rs'` is not recursive without `:(glob)` and silently reaches a
  fraction of the files.
- Was any test run believed on exit code alone? Require `running N tests` **and**
  `test result` in the output. A background task's reported exit code is the
  **pipeline's** (`grep`/`tail`), not the command's — a failed run notifies green.
- Was any negative finding ("X does not exist", "nothing calls this")
  load-bearing? Those are the ones that get fabricated. Re-verify each one
  yourself; do not accept a subagent's report at face value.
- Did anything claimed as "verified" inspect the working tree instead of the
  committed one?

Report conformance per policy clause, by clause name — not as a summary grade.

## Step 2 — Convert every DROPPED item into a durable store

For each DROPPED item, route it to a store. Try in this order, and stop at
the first that fits. A finding may **accompany** a gate or a memory where the
sections below say so explicitly; what it must never do is **substitute** for
one.

### Probe before you re-derive

Before asserting any **environmental** claim in this step or the next — a door
is 404ing, a checkout cannot be pulled, a cache has never rendered — call
**`coord_recent_findings`** for the `resource_keys` you are about to touch (and
the `topic`, when you know the subsystem before you know the files). Findings
are pull-by-relevance: nothing pushes them at you, so a session that never asks
is told nothing.

This clause exists because a real session re-derived a standing
`/api/v1/plan-library` 404 from scratch **while a memory line naming that exact
404 was already in its context**. Knowing a thing and being told to look are not
the same, and this command previously told you to look nowhere.

If a returned finding already covers the condition, **cite it and move on** — do
not repeat the investigation. If your own evidence *corrects* it, post the
replacement with `supersedes` set to the stale `finding_id`, so reads return one
live head rather than two contradictory rows.

### 2a — Is it WAITING on an observable condition? → a coord gate

If the reason it is unfinished is a condition coord can watch flip, register a
typed gate. **Use `/blocked`** — it is the canonical session-close procedure and
already carries the predicate-selection table and the registration cascade
(`_gate-registration` is the spec both implement).

Hazards that make a gate *look* registered when it is not, each of which this
step must actively defeat:

- A coord write returning **"Command failed with no output"** is a dead cached
  transport, and the write is **presumed LOST**. Run `/coord-revive`, re-issue
  over the live door, and **verify by read**.
- ~~**`pr_merged` … never fires at all on a coord-orchestrated repo (ff-land
  closes with `mergedAt:null`)**~~ — **both halves corrected.** The gate does
  not read GitHub's `merged` bool at all: `gates::pr_merged_verdict` clears on
  coord's own land record (`pr_state = 'merged'` **or** `close_cause ∈ {merged,
  commits_landed_via_other_pr}`), so it clears on a coord land, and a coord land has **two**
  GitHub shapes anyway — `CLOSED, merged=false` only when the rebase rewrote the
  sha, `MERGED` with `mergeCommit.oid == headRefOid` when it did not
  (`knowledge-base/qontinui-specific/coord-ff-lands.md`). What survives is
  narrower and is **not** a false clear: an explicitly `open`/`draft` `pr_state`
  carrying a land cause hits the **contradiction guard** and returns `Open`
  until the PR leaves open/draft. The guard is deliberately narrow — `pr_state =
  None` plus a land cause is **not** a contradiction and still clears: that is
  coord's ff-land record beating the webhook, and the content really did land.
  The real coin-flip is the **pre-land SHA
  anchor** — a `commit_live`/`ref_exists` gate on the branch head you read
  before landing clears only on the sha-preserving shape — so anchor
  `commit_live` to a **post-land main SHA**, never a pre-land one.
- **`file_exists` 403s fleet-wide.** Do not use it.
- ~~**`unit_status` is rejected 400/422 over the device HTTP door**~~ —
  **corrected 2026-08-23: it is accepted.** Registering
  `{"kind":"unit_status","work_unit_id":"<uuid>","status":"shipped"}` over
  `POST $COORD_HTTP_URL/coord/work-units/<slug>/register-gate` with a
  runner-minted **device JWT** returned a `gate_id` with an empty `warnings[]`,
  and the gate evaluated on schedule (`verdict: open` against an `in_progress`
  unit, `continuation_will_dispatch: true`) rather than pinning unevaluated.
  Keep the rest of the hazard: a **`pr_merged` on a PR closed WITHOUT a land
  cause** (`author_closed` / `unexplained` / `branch_deleted_by` / NULL) **goes
  terminal `failed`, un-withdrawable** — a PR closed *with* a land cause clears
  normally. **The no-land-cause class includes the dying-land arm**, where
  the land pushed and flipped the proposal but died before the provenance
  stamps — it reaps `closed` with `close_cause = 'unexplained'`, which is why it
  lands in this class — so the gate reads `Failed` on work that is provably on
  `main`. Content
  on `main` is **not** the discriminator there (a routine `author_closed` whose
  work shipped via a different PR looks identical); a `merged`
  `coord.merge_proposals` row for **this** PR is. That is a reason to check a
  `Failed`, **not** a reason to avoid the predicate on a coord-orchestrated
  repo — it clears there normally. **`unit_status` on the plan's own work unit is
  usually the right one for "resume when this plan's work has landed"**, because
  `shipped` is DERIVED from merged PR citations.
- **`Misconfigured` is terminal** — the sweep re-evaluates `open` only. A
  wrong-Open gate self-heals; a wrong-Misconfigured one is permanent.
- A blocker with **no observable trigger is not a gate.** Do not force-fit a
  predicate onto an open-ended TODO; that pollutes the registry with gates that
  never clear. Route it to 2b, 2c or 2d instead.
- **Send `gate_class`, and read `initial_verdict_reason` back.** These are the
  two registration mechanics this step is a consumer of, and omitting either is
  invisible at registration time. `gate_class` decides **who may clear** the
  gate — omit it and a closeout silently files unclassified gates, which is how
  the clearance-authority surface stayed dark fleet-wide for a week. An
  `/unattended` gate is a session-close artifact with no operator watching, so
  classify it BY DEFAULT rather than leaving it to a later reader.
  `initial_verdict_reason` is what separates a REGISTERED-BUT-NOT-USABLE gate
  from a usable one: a returned `gate_id` is **not** sufficient. Treat a
  non-empty `warnings[]`, or an `initial_verdict_reason` containing **"cannot
  evaluate"**, as **not registered** for the purposes of Step 2a — the item is still DROPPED
  and must fall through to 2b/2c/2d. **One exception, and it is not a judgement
  call:** a `pr_merged` gate on a coord-orchestrated repo *always* comes back
  carrying coord's informational `ℹ … the clear may lag GitHub's close by one
  provenance write` steer, which `check_predicate` pushes into **`warnings[]`**
  and not only into `steer` (`gates.rs`, `pr_merged_orchestrated_warning` →
  `warnings.push`). That is a steer, **not a rejection**: the gate is registered
  and usable. Read the warning text before dropping anything — the rule is
  "warnings mean not-usable", and this one says the opposite in words.
  Canonical: `_gate-registration`, which lists this command as a consumer.

Read every `gate_id` back. **A gate you did not read back is still DROPPED.**

### 2b — Is it a condition that is currently TRUE, environmental and shared? → a coord finding

`coord.findings` is the cross-session knowledge feed this fleet already ships,
and it describes itself as *"the tier between a session's private transcript and
the permanent, distilled `MEMORY.md`: raw recent investigation, TTL ~14 days,
resource/topic-scoped, auto-expiring"*. A closeout is exactly what it was built
to receive; this command simply never offered it. A sibling closeout
(`/cleanup-steward`) has been routing to it all along.

Route here when the item is a **standing condition of the environment** rather
than work to resume or a durable fact — a door that is 404ing, a checkout that
cannot be pulled with peers holding WIP, a capability that is unavailable *right
now*:

```
coord_post_finding(
  kind          = "status" | "gotcha",
  scope         = "tenant" | "fleet-infra",
  topic         = "<subsystem, e.g. plan-corpus / merge-engine / coord-deploy>",
  resource_keys = [ "<the files/globs/PR/plan-slug a peer would be working>" ],
  title = "<one line>", body = "<a few sentences — more than a memory>")
```

`kind="status"` for *this is how the world is right now*; `kind="gotcha"` for an
operational trap a peer will walk into. `scope="fleet-infra"` **only** when the
condition is a truth about **shared** coord/runner behaviour — anything
tenant-local stays `scope="tenant"` (the default). `resource_keys` is not
decoration: it is the overlap key a peer's `coord_recent_findings` matches on, so
a finding filed with none is a finding nobody will be handed.

**The split against 2c, stated so nothing routes twice.** Findings **expire in
~14 days**. That makes them right for what is *true now and expected to become
false*, and wrong for what will still be true next quarter. A permanent hazard —
an interface that will always mangle Windows paths, a probe that is vacuous by
construction — belongs in **memory**, distilled. Routing everything to findings
would quietly delete the durable half a fortnight later; routing everything to
memory buries today's transient condition among permanent ones. One item may
legitimately produce **both**: the *condition* as a finding, the *lesson* as a
memory. What it must never produce is a finding standing in for a memory.

**Transport — findings are MCP-only today, and MCP is the transport that dies
first.** Measured: a session held a working coord **device JWT** for its whole
run — work-units, gates, prompt-documents and the `/pr-merge` feed all answered —
while `/coord-mcp` returned `401` and the workspace `.mcp.json` had been deleted
outright by an ephemeral runner. That is precisely the degraded session most
likely to be holding a fleet-infra condition worth recording. So try, in order:

1. **`coord_post_finding`** when the tool is visible **and a call answers**.
   Those are two different things through the runner's `/coord-mcp` proxy:
   `tools/list` is unfiltered while `tools/call` is gated by
   `COORD_MCP_ALLOWED_TOOLS`, so a `-32601` means *not callable here*, not
   *not shipped*. (Both findings tools are on that allowlist today —
   verified — so this arm works; the point is not to read visibility as
   proof.) If the call returns `"Command failed with no output"`, that is a
   dead cached transport and the write is **presumed LOST** — `/coord-revive`,
   re-issue, verify by read.
2. **The device-authed HTTP twin** — `POST /coord/agent-findings`, read side
   `GET /coord/agent-findings?resource_keys=…&topic=…` — on the same
   `require_jwt` sub-router as `agent-work-units` and `agent-gates`. It is being
   added by Phase 2 of
   `2026-08-23-impediment-registry-and-unattended-as-knowledge-substrate`; a
   `404` from it means it has not landed on the **serving** build, which is arm 3
   below, not a retry loop.
3. **Neither answered → say the finding was NOT recorded.** Name the transport
   failure you actually saw, and leave the item **DROPPED**.

**Never report a finding as stored on the strength of having issued the call.**
`coord_post_finding` answers
`{"posted": false, "reason": "coord.findings is not provisioned yet …"}` when the
table is unprovisioned — a **200 that stored nothing**. Per this command's own
rule, an item with no returned `finding_id` is DROPPED, not IMPEDED.

Read that id from the right place: success is
`{"posted": true, "finding": {…, "finding_id": …}}`, so the id is **nested
under `finding`**. A session that reads `.finding_id` off the envelope gets
nothing and declares DROPPED on a write that actually succeeded — the mirror
image of the failure this paragraph is about.

**Edges by convention — no schema is being added.** Record dependence in the
finding **body** as `blocked_by: <slug>` and `causes: <slug>` lines naming the
other findings (with their `finding_id`s where you have them).
`coord.findings` has no edge column and none is proposed; the convention is what
makes *"what is blocking plan dedup?"* answerable at all. Do not invent a field
name that reads like schema — a fabricated column is worse than prose, because
the next session will query it.

The same pair belongs in `alerts.detail` when the condition is *also* carried by
an alert — but **you cannot write it from a session**: every `INSERT INTO
coord.alerts` is a coord-internal watcher, and there is no `coord_post_alert`
tool and no alert-write route on the agent door. Reading is barely better for
this class, and **the fix that was supposed to change that has landed without
changing it.** `GET /coord/alerts` accepts a device JWT; coord#1601 (`61a107be`,
Phase 2/G2 of the plan above) added a machine arm so a device principal could
read the infra class, scoped to a closed kind allowlist
(`FLEET_INFRA_MACHINE_KINDS`) AND the no-device/no-tenant/no-repo shape. But the
arm is gated `!strict_tenant` in BOTH `build_get_alerts_query` and
`fleet_health_rollup_sql`, and production runs `COORD_ALERTS_TENANT_STRICT=1`
— measured 2026-08-25 against the running service `qontinui-staging-coord:857`,
and there is no separate staging. So the arm never fires in the only environment
that exists, and infra-scoped alerts remain unreadable from an ordinary tenant's
session exactly as before. **Read that as UNKNOWN, not as "no alert exists"**
[policy: `verification-and-evidence` `silent-empty-is-unknown`]. Do not "verify"
this by finding that you CAN see NULL-`device_id` rows: an operator/system-tenant
device reaches that class through a pre-existing arm, so a privileged principal
is not a sample. So the finding body is the edge record you
actually author; the `alerts.detail` mirror is a note for whoever owns the
alert, not a step in this procedure. Never report an edge as mirrored when no
door existed to mirror it through.

**This is not a gate, and must not become one.** A finding *records* a condition;
a gate *watches an observable trigger* and resumes the work when it flips (2a,
`/blocked`, `_gate-registration` — gates remain for observable triggers only). If
the condition has a predicate coord can evaluate, it belongs in 2a, and you may
post the finding **as well** so peers know it is true today. If it does not, the
finding is the honest store, and force-fitting a predicate onto it pollutes the
registry with gates that never clear.

### 2c — Is it a durable fact, correction, or hazard? → the memory store

If the item is knowledge rather than work — a non-obvious property of the
system, a trap that cost this session time, a correction to a belief this session
started with, an approach that was confirmed — record it.

- When **`coord_memory_record` is visible**, author through it. Kind mapping:
  `feedback`→`feedback`, `reference`→`reference`, user-fact→`fact`,
  project-state→`observation`. Redaction, dedup and quotas are enforced
  server-side, so pre-filtering beyond normal secret hygiene is redundant.
- When it is **masked**, fall back to the file workflow unchanged: a topic file
  plus a **one-line** `MEMORY.md` index entry. One target per line — never join
  entries, since the union merge dedups per line and a grouped line doubles the
  index on the next sync.

What belongs here is the thing that was **non-obvious**. What does not: code
structure, git history, anything already in `CLAUDE.md` or the knowledge base,
and anything that only mattered inside this conversation.

Before writing, search for an existing record covering it and **update that**
rather than adding a duplicate. `coord_memory_search` is **full-text only**
unless the call carries a query vector — read the response's `vector_arm` field
rather than assuming, and phrase the query in the target record's own words while
it reads `skipped_no_embedding`. A missing answer is **UNKNOWN, not "no
memories"**.

### 2d — Is it deferred implementation work? → the plan corpus

Work that is neither waiting on a trigger nor merely knowledge — it needs doing,
later, by someone — belongs in a plan. Hold it for Step 3, which first checks
whether it is already built or already planned.

### 2e — Nothing could hold it

If an item fits **none** of the above — there is no store in qontinui shaped to
receive it — **stop and record that fact**, because it is the highest-value
output of this whole command. It is a **capture gap**: the loop lost information
not through your error but because the system has nowhere to put it. Carry it
into Step 3 as a named gap.

## Step 3 — Gap-driven capability sweep, then authoring

This step is **bounded by the gap list from Steps 1–2**. It is not a standing
backlog report. If Steps 1–2 produced no capture gaps and no deferred work, say
so in one line and skip to Step 4 — a clean session must close cheaply.

For each named gap, resolve in this order. **Stop at the first hit** — the order
is cheapest-first, and skipping ahead is exactly how a plan gets authored for
something that already shipped behind a flag.

### 3a — Is it already BUILT but not ACTIVATED?

Sweep for functionality that exists in shipped code but is off: feature flags,
env-gated arms, dry-run/shadow modes, dark routes, unwired exports.

> **Read the deployed configuration, never the in-code comment.** Comments
> describing an arm as "shadow" or "dry-run" go stale the moment the flag is
> flipped in deploy config, and are a documented source of wrong conclusions in
> this fleet. Check the deploy task definition / environment of the **running**
> service, and prefer probing a **feature-marker field** on a live endpoint over
> trusting `git_sha` (served empty) or a `buildId` you have not mapped to a
> commit.

If the capability exists and is merely off, the deliverable is **not a plan** —
it is an activation, plus a gate if flipping it waits on an observable
precondition or an operator decision.

### 3b — Is it already PLANNED but not IMPLEMENTED?

Query the plan corpus. **The DB is authoritative for reads** — discovery, search
and selection resolve against `agent.work_artifacts` behind qontinui-web
(`/api/v1/plan-library/*`), not against a directory. `$QONTINUI_PLANS_DIR` is an
**authoring surface**, and being unset is a supported configuration, not an
error.

Degraded read when qontinui-web is unreachable: `$QONTINUI_PLAN_CACHE_DIR`
(default `C:/claude/plan-corpus-cache/`) — `PLANS-CACHE.md` for the index,
`bodies/<kind>__<slug>.md` for bodies. Refresh with
`qontinui-claude-config/scripts/render-plan-cache.ps1 -MaxAgeHours 0`.

**Say plainly which surface you read, and quote its `Rendered` stamp.** Read the
`PLANS-CACHE.state.json` sidecar: a `rendered_at` of `null` means the cache has
**never** rendered, and a stale or absent cache is **UNKNOWN, never empty**.
"This render did not see it" is not "it does not exist" — and reporting a missing
plan as absent is precisely how the same plan gets authored twice.

**A corpus that ANSWERS is not a corpus that is POPULATED.** The plan-library
body sync (`body_push.rs` -> `agent.work_artifacts`) is **opt-in**: it is built
only under `QONTINUI_PLAN_LIBRARY_SYNC=1` (`trigger.rs` `body_sync_enabled()`)
and gated again per cycle on the tenant's `plan_capture` fleet dial, and
**either missing is a silent no-op**. The operational layer
(`coord.work_units`) fills regardless, so the two layers diverge with nothing
logged - measured 2026-08-22 on the operator box: 343 plans scanned, all
work-unit rows correct, and **zero** rows in `agent.work_artifacts`. A `200`
carrying an empty list is therefore **UNKNOWN, not "no such plan"** - it is the
frozen-corpus signature, and it is the one path that reaches 3c **without any
door ever failing**, so the UNKNOWN clause below does not catch it on its own.
Treat any zero-result corpus read as UNKNOWN unless you have positively
confirmed the body sync is on for this device.

**Do not probe by stem with `q`.** `GET /api/v1/plan-library?q=` matches **title and
body, NOT the slug** (measured 2026-08-22), while the stem is the canonical plan
identifier everywhere else - `Depends-On:`, the `Plan: <stem>` PR marker,
`$QONTINUI_PLANS_DIR` filenames. A by-stem existence probe therefore returns a
**false negative for a plan that is present** - another definite-looking "no"
that routes straight to 3c. **The exact by-stem door is
`?kind=plan&work_unit_slug=<stem>`** - the adapter writes a plan's own stem into
`work_unit_slug` (`body_push.rs:558`, `kind == Plan` only) and the list route
filters that column exactly. Failing that, page `?kind=plan&limit=200` and match
the `slug` field yourself; the list route has no `slug` filter, and a zero from
either is still UNKNOWN under the frozen-corpus rule above.

**An UNKNOWN at this step is not a miss — it must NOT fall through to 3c.** If
neither the corpus nor a *successfully rendered* cache answered, you have not
established that the gap is unplanned; you have established that you cannot tell.
Record it as `PLAN-STATUS UNKNOWN`, name the door that failed, and treat the
unreadable corpus as a **capture gap in its own right** (Step 2e) — deferred work
cannot be routed to a store you cannot read. Authoring against an unreadable
corpus is exactly the duplicate-authoring failure this step exists to prevent.

**An UNKNOWN with a known cause is not a mystery — report it AS its cause.**
Every UNKNOWN in this step comes from some door that did not answer, and that
door's condition is usually already recorded: either as a finding you posted in
2b, or as one `coord_recent_findings` returned to you before you started. Cite
it. The line reads *"PLAN-STATUS UNKNOWN — finding `<finding_id>`: the plan
corpus has never rendered on this box"*, not *"the corpus did not answer"*. The
second phrasing invites the next session to re-derive the same 404 from scratch,
which is the exact failure the probe-first clause above exists to stop; the first
hands it the answer. Only an UNKNOWN with **no** covering finding is a genuinely
new capture gap — and it is one you should post as a finding before Step 4, not
merely narrate in the report.

Two further traps when judging whether a plan is implemented:

- **A plan's own `Status:` stamp is not evidence.** A large share of shipped work
  still reads PROPOSED / DRAFT / IN PROGRESS on disk. Check the coord work unit
  and its PR citations instead. Do **not** mass-restamp disk plans as a side
  effect of this audit.
- **Re-count the inventory at implement time.** `main` moves; a count taken at
  vet time is stale by the time anything acts on it.

If a plan already covers the gap, the deliverable is a status correction and, if
it is genuinely ready to dispatch, a `unit_ready` gate — **not** a new plan.

### 3c — Neither → author the plan

Enter 3c **only when 3a and 3b each returned a definite no.** An UNKNOWN from
either — an unreachable deploy config, an unreadable corpus, an unrendered cache
— routes to the UNKNOWN handling above, never here.

Only now author a new plan. It must state the **capture gap** it closes, in those
terms: what information the loop lost, where it should have been stored, and what
surface is missing. A plan that describes only the symptom ("a session forgot X")
will not survive vetting.

Before writing a line, verify the plan's own stated preconditions by grepping the
repo. A plan premise of the form "X is impossible without Y" is frequently just
**false**, and vetting line numbers against a false premise burns rounds that one
`ls` would have saved.

Authored plans flow into the corpus by the normal path. Do **not** run `/vet-imp`
from inside this command — that starts an implementation session, and this is a
closeout.

## Step 4 — Report

Lead with the verdict, one line, in this command's own terms:

> **Unattended verdict: COMPLETE / INCOMPLETE — N units, M converted, K still DROPPED.**

Then:

1. **The classification table** — every unit, its terminal state, and its
   evidence (landed SHA, `gate_id`, `finding_id`, memory id, plan slug). **A
   row without evidence is DROPPED**, regardless of what it claims.
2. **Policy conformance** (Step 1b) — per clause, with the document versions read
   in Step 0.
3. **Conversions performed** — gates registered *and read back*, findings
   posted (each with the transport that actually carried it), memories written,
   plans authored; each with its identifier.
4. **Capture gaps** — the Step 2e items, and for each one whether 3a / 3b / 3c
   resolved it. This is the holistic signal: a gap that recurs across sessions is
   a standing defect in qontinui's automation loop, and naming it is the point of
   running this at all.
5. **Residual DROPPED** — items you could not convert, each with the reason and
   the specific failure you observed. **Never report an empty residual list you
   did not verify.**
6. **Surfaces read, and their freshness** — which doors answered, which were
   degraded, and the stamp on any cache you relied on. A verdict computed from a
   stale cache is a stale verdict and must say so.
7. **Plan-corpus reachability - report this even when Step 3 never ran.** Quote
   `PLANS-CACHE.state.json`'s `render_exit_reason` and `rendered_at` as a
   one-line fact. Step 3 is gap-driven, so a clean session never looks at the
   corpus at all; this line is then the only thing standing between the fleet
   and a corpus that has been dead for days. A `rendered_at` of `null` means it
   has **never** rendered. Report it; do not investigate it here.

### Honesty rules for the report

- **Absence is UNKNOWN, not zero.** Every unreachable door, silent-empty probe
  and unrendered cache is reported as UNKNOWN, with the failure named.
- **State the reach of every sweep.** A bounded or partial sweep never reads as a
  complete one.
- **A conversion is not done until it is read back.** Report what you verified,
  not what you issued.
- If this command's own procedure was the thing that failed, say so — and treat
  that as a capture gap in Step 3 like any other.
