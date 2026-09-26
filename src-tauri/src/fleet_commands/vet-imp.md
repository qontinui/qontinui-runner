# Vet & Implement Plan

Run `/vet-plan` on a plan, then — once it's stamped VETTED — immediately run
`/implement-plan` on the same plan. One command for the full
**vet → implement** lifecycle in a single session, no stop in between.

This is a thin orchestrator: it does not re-implement any of the vetting or
implementation logic. It invokes the two canonical skills in order and passes
the resolved plan path through. All coord wiring (status publication, claim
pre-flight, `unit_ready` gate registration, the VETTED/IN PROGRESS/SHIPPED
stamps — applied where the plan lives) is owned by those two skills — do
not duplicate it here.

What it does check for itself is the **presence** of what a skill was required
to produce. Step 5 will not report the chain complete when the implement half
opened PRs and left no pre-PR-review artifact behind. That is a **gate on the
hand-off**, not a second copy of the review: the registry read, the four
disposition branches and the artifact write all live in `/implement-plan`, and
re-stating any of them here would create exactly the divergeable second copy
this rule exists to prevent.

**One exception, and it exists because this command mutates first.** Step 1
commits and pushes an untracked plan file *before* `/vet-plan` is ever invoked,
so this command has a write of its own ahead of both skills. Exclusion must
precede mutation, so the **plan reserve** (Step 1.1) is taken here, by the
orchestrator, on the same key the two skills use. Both then re-issue the same
call and get a **renewal**, not a conflict, because the owner token is the same
harness session — that is the mechanism that makes one reserve span the whole
chain instead of three fighting over it.

It also **labels the session** so the vet → implement run is easy to find in
the session list: a provisional plan-slug label at the start (Step 1.5), then a
final PR-numbered name at the end via the `/name` command, once implementation
has opened the PRs (Step 5).

## Arguments

- `$ARGUMENTS` — Path to the plan file (relative or absolute). Optional. If
  omitted, resolve the plan the same way `/vet-plan` does: look for the most
  recently modified `*.md` under `$QONTINUI_PLANS_DIR` (see below) and the
  working-tree root, and confirm the choice with the user before vetting.

  Any trailing flags `/implement-plan` understands (e.g. `--wait-timeout=<Nm>`)
  are forwarded verbatim to the implement step; they are ignored by the vet
  step.

## Plan directories

This command does not resolve plan directories itself — `/vet-plan` and
`/implement-plan` each document the full contract, and Step 1 hands both of them one
already-resolved **absolute** path. The only directory this command touches is the
omitted-argument fallback above:

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

- **`$QONTINUI_PLANS_DIR`** — the directory plans live in. The qontinui runner injects
  it into agent sessions from its `paths.plans_dir` setting; a session launched outside
  the runner will not have it. **If it is unset, ask the user once where plans live, or
  DISCOVER one: from the workspace root, `ls -d plans */plans 2>/dev/null` and use the
  directory that actually exists** — say which, and ask when it finds none or more
  than one. Never fall back to a directory you have not confirmed is there; a named
  fallback fails silently on every machine that does not have it. Never assume an
  absolute path from another machine.

Where a plan file finally *lives* after shipping — stamped in place by default — is
`/implement-plan` Step 6's call, not this command's. Do not move plan files here.

## Instructions

### Step 1 — Resolve the plan path once

Determine the single plan file this run operates on (from `$ARGUMENTS`, or via
the most-recently-modified fallback above, confirmed with the user). Hold this
resolved absolute path; both downstream skills receive the **same** path so the
vet stamp and the implement run can never drift onto different files.

If that file is still untracked in git, commit and push it — stamped `DRAFT`,
from a worktree, never the primary/shared checkout — before Step 2. `/vet-plan`
documents the same precondition: `VETTED` is an attested status a non-owner
session must be able to read, so vetting a file no peer can see defeats the
attestation. **That commit-and-push is a mutation, so it happens AFTER Step 1.1's
reserve, not here** — resolve the path in this step, reserve in Step 1.1, then
push.

⚠️ **"From a worktree" decides WHERE YOU COMMIT; it does not decide where the
plan LANDS, and only the second one satisfies the reason above.** A worktree
sits on its own branch, so a bare `git push` there puts the plan on that branch
and nowhere else — still a file no peer can see, which is precisely the failure
this precondition exists to prevent. Land it on `origin/main` and **read it
back** before treating the precondition as met. The read-back is a content
comparison: the file's hash must equal `origin/main`'s blob at its path, as in
`/implement-plan` Step 6 item 3's read-back with its non-empty guard. Existence
alone passes on an earlier version already at that path. Use the same
throwaway-worktree recipe
`/implement-plan` Step 6 item 3 spells out, which also carries the
`closeout-push` authority and the non-fast-forward retry. A plan that fails the
read-back is not vettable yet — say so rather than proceeding to Step 2.

**Where the plan repo is itself coord-merge-authority, the direct land is not
available and a PR is the only route — then the assertion is that a PR
carries each push, re-checked before and after it** (per the
`coord-ff-lands.md` section named below). A pushed branch with no pull request
never reaches `main`, so the plan is on `origin` and still invisible to every
`origin/main` reader. Measured 2026-09-02, **9** plan stems were pushed to
`origin` and never proposed at all — no PR in any state, on any branch carrying
the stem. A PR that existed at Step 1 is not enough either: coord can land it
mid-chain and leave it CLOSED, MERGED or even OPEN, stranding every later push
to the same branch. So before each push, and again after it, apply
`knowledge-base/qontinui-specific/coord-ff-lands.md` → "Pushing to a branch
whose PR may already have landed", reading the PR with
`gh pr list --repo <owner/repo> --head <branch> --state all --json number,state,headRefOid`.
When it says no PR carries the push and commits remain unlanded, take its
fresh-branch path and open the new PR — **`coord_create_pr` first, then
`gh pr create`** — with a line-anchored `Plan: <stem>` marker in the body,
carrying the DELIVERY SCOPE for the phases this PR actually implements
(`Plan: <stem> phases: 2,3` — `/implement-plan` Step 4.5 has the grammar and the
reason). **Where this skill used to WITHHOLD a citation because the PRs deliver
only part of the plan, cite it with a scope instead.** Withholding never worked:
the webhook auto-captures the `Plan:` marker from the PR body and those captures
are not removable, so a withheld citation was recorded anyway — measured
2026-09-05 on 40 partially-delivered plans, all 40 auto-cited. A scoped citation
is the supported way to say "this PR delivered phases 2 and 3 and no more"
*(plan `2026-09-13-coord-delivery-cannot-express-partial-delivery`)*.
**Never `gh pr merge`, never `--admin`** — coord is the sole merge authority.
The marker is an **indexability** claim, not a delivery one: once plan
`2026-09-04-docs-only-plan-marker-prs-derive-shipped` Phase 1 is deployed
(authored 2026-09-04, not deployed as of that date), coord classifies a citation
whose PR changed only plan documents as a *document citation*, which neither
derives nor blocks `shipped` — so marking a plan-file-only PR cannot forge it.
Runbook:
`knowledge-base/qontinui-specific/bodyless-work-units-and-stranded-plans.md`.

### Step 1.1 — Reserve the PLAN in coord (before the first write)

The plan path is resolved and nothing has been written yet. Reserve the plan
**now**, before Step 1's untracked-plan commit-and-push and before either skill
runs. Everything after this point mutates.

This is the same reserve `/preflight` step 0 specifies
(`.claude/skills/preflight/SKILL.md` → "0. Reserve the plan (free today — do this
FIRST)"), on the same key `/vet-plan` step 0.2 and `/implement-plan` Step 0.48
use. Wiring all three lifecycle commands to one protocol makes **`/preflight`
load-bearing for the entire plan lifecycle** — the accepted trade: one
implementation to keep correct beats four that drift.

**Granularity.** The key is the **plan**, not a phase: `plan:<plan-stem>` means
*"this document is mine to move."* `/implement-plan` Step 0.6's
`plan:<plan-stem>:phase:<n>` claim is a nested second granularity meaning *"this
phase's agent is mine to spawn."* A vetter and an implementer share no phase
number, so only the plan key can ever see them collide — measured 2026-08-25,
**14 strict duplicate-PR pairs in 60 days** (one MERGED + one CLOSED-unmerged,
< 4 h apart, ≥ 2 shared files, Jaccard ≥ 0.50).

#### Resolution

1. **`<plan-stem>`** — the Step 1 path's filename without `.md` or directory
   prefix. The canonical cross-agent key `coord.plans`,
   `coord.sessions.plan_slug`, `unit_ready` gates, the `Plan: <stem>` PR marker
   and `Depends-On:` all use. It is also the Step 1.5 slug's input, before the
   date prefix is stripped — reserve on the **full** stem, date included.
2. **Machine UUID** — env `QONTINUI_MACHINE_ID` first; else
   `~/.qontinui/machine.json` parsed for **`"device_id"`** (canonical
   post-unified-devices), falling back to `"machine_id"` if present (legacy
   shape). **The wire field does not follow the local key:** `/claims/*` takes
   `machine_id`, `POST /coord/status` takes `device_id`. Send the same UUID under
   whichever field the route names.
3. **`AGENT_SESSION_ID`** — resolve once, here, and reuse for the release:

   ```bash
   AGENT_SESSION_ID="${QONTINUI_AGENT_SESSION_ID:-${CLAUDE_CODE_SESSION_ID:-}}"
   ```

   The two skills resolve the identical value from the identical env vars in the
   same harness session, which is exactly why their re-reserves renew rather than
   conflict. If both are empty (older Claude Code), omit the field; never send an
   empty string.
4. **Coord HTTP base** — `COORD_HTTP_URL`, else `https://coord.qontinui.io`.

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
    "skill": "vet-imp"
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
match on it, qontinui-claude-config PR #49 sends it) fixed for phase claims. It is
also what makes the nested renewals below work at all: without an owner token,
`/vet-plan`'s own reserve would be indistinguishable from a peer's.

#### Branch on the result

- **`granted` / `claimed`** — a fresh reservation. **This chain is the OWNER and
  therefore the releaser** (Step 5). Proceed to Step 1's push, then Step 1.5.
- **`renewed`**, or a **`held` whose `current_holder_session` equals your own
  `$AGENT_SESSION_ID`** — this session already holds it. Proceed, and leave the
  release to whoever acquired it.
  **`renewed` is only as narrow as the door's owner token.** Over
  `coord_reserve_resource` (MCP) that token is the bare DEVICE, so a second
  session on this same box re-reserving the plan ALSO reads `renewed`; only the
  HTTP `/claims/acquire` fallback, which carries `agent_session_id`, makes it
  session-scoped. So a `renewed` is YOUR hold only if something earlier in THIS
  session reserved the plan (you are nested under `/vet-imp`, or this run
  re-reserves its own key). If nothing did, treat it as `held` by a same-box
  peer — the rule below.
- **`held` by a DIFFERENT owner — STOP.** Do not push, do not invoke `/vet-plan`.
  Report the holder and surface to the operator via `AskUserQuestion` (header
  `Plan reserved`, options **Abort** / **Wait** — poll every 30 s, then
  re-acquire). When `current_holder` equals THIS machine and
  `current_holder_session` differs, say so explicitly rather than implying a
  different box:

  ```
  Another session on THIS machine (session <current_holder_session>) already
  holds plan:<plan-stem>.
  ```

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
  do and it names siblings, name them in the Step 5 report and pass them into
  `/vet-plan` as context. If an older coord build still answers `fork_risk` or a
  non-empty `forking_siblings`, read it as the advisory overlay it always was —
  name the siblings and pass them through — never as a hard holder.
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

**Second, a timed-out WRITE is not a write that did not happen.** The
lost-write doctrine elsewhere in this fleet is written for a *dead transport*
(`"Command failed with no output"`), where presuming the write LOST and
re-issuing is right. A **client-budget timeout is a different failure**: the
request reached coord, coord committed it, and only the response was lost. On
2026-09-03 a `register-gate` POST in this chain exceeded its client budget with
no body, and read-back showed the gate had been created — `created_at`
`09:15:55.745`, 1.1 s after the call. Presuming that LOST and re-issuing would
have written a **second gate on the same anchor**, which §5.4's own
"refresh, don't duplicate" rule exists to prevent.

So for any coord **write** in this chain that times out with no body — the
reserve here, and the work-unit upsert, status transition and gate
registration the two sub-skills issue — **read it back BEFORE re-issuing**, and
branch on the row, not on the absent response. The reserve itself is safe
either way (a re-acquire by the same owner token returns `renewed`), but the
sub-skills' writes are not: a duplicate gate, or a transition replayed over a
peer's, is a real defect. `"Timed out"` is UNKNOWN, and UNKNOWN is resolved by
reading, never by assuming either direction — served policy
`verification-and-evidence` `unknown-must-not-render-as-a-default`.

> **Coord unreachable** (connection error, timeout, non-2xx, unparseable body)
> on a device that DID resolve a machine UUID: this is **UNKNOWN, not free.** Do
> not push and do not invoke `/vet-plan`. Report the transport failure verbatim,
> run `/coord-revive`, and re-issue over the door it reports LIVE. If no door is
> live, surface to the operator via `AskUserQuestion` (**Abort** / **Proceed
> uncoordinated**) — proceeding is a decision someone makes, never a default
> reached by falling through an undocumented branch.

**Skip-and-warn only when there is no machine UUID at all.** If neither
`QONTINUI_MACHINE_ID` nor `~/.qontinui/machine.json` supplies one, emit a
single-line warning (`⚠️ plan reserve skipped: no machine_id available — running
the chain without reserve coordination`) and proceed. The asymmetry is
deliberate: a device with no machine UUID **cannot participate in coordination at
all** (permitting it is a stated trade), whereas a registered device that merely
cannot *reach* coord is a full participant whose peers are invisible — the case
where proceeding is most dangerous. Same observable ("no reserve acquired"),
**opposite** correct response. Collapsing them is the `silent-empty-is-unknown`
class (served policy `verification-and-evidence`) applied to a mutex.

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

#### Keep the reserve ALIVE — start the heartbeat, on `claimed` only

A `semantic_resource` reserve has a finite TTL and coord evicts it when nothing
heartbeats it. A `/vet-imp` chain routinely runs longer than that TTL, so the
reserve this step just took **lapses mid-chain unless something heartbeats it** —
and the first symptom is a `not_held` at Step 5, by which point the plan has been
unprotected for hours and a peer could have taken it.

`scripts/coord-claim-heartbeat.sh` owns that loop. Start it **only when the
reserve returned `claimed` / `granted`** — the acquirer owns the heartbeat
exactly as it owns the release. On `renewed` (or a `held` whose
`current_holder_session` is your own) do **nothing** here: the chain that
acquired the reserve is already heartbeating it, and a second loop on one key
only doubles the request rate.

```bash
# Ledger path — resolve ONCE here and reuse for remove / stop / status below.
CLAIM_LEDGER="$HOME/.qontinui/claim-ledger/${AGENT_SESSION_ID:-nosession}.ledger"

# --ttl is the `ttl_seconds` coord GRANTED, copied off the reserve response
# this step just received (both doors return it) -- never a guess. The
# loop's cadence is derived from it and `add` refuses without it.
bash <workspace-root>/qontinui-claude-config/scripts/coord-claim-heartbeat.sh add \
  --ledger "$CLAIM_LEDGER" \
  --kind semantic_resource \
  --key "plan:<plan-stem>" \
  --ttl "<ttl_seconds>"
# --max-runtime 604800 (7 days): this is the LONGEST lifecycle chain of the
# three — session 93078f26 ran one vet-imp-style effort for twelve days and
# lapsed five hand-raised ceilings (12h, 12h, 24h, 48h, 168h) before it gave up.
bash <workspace-root>/qontinui-claude-config/scripts/coord-claim-heartbeat.sh start --ledger "$CLAIM_LEDGER" --max-runtime 604800
```

`start` detaches a background loop that re-heartbeats each row when THAT row
falls due — every min(max(its own TTL/3, 60 s), TTL/2) since its last ok —
replaying the owner token
`<machine_id>:<agent_session_id>` on every request — coord matches on that pair,
so a heartbeat without it does not renew anything and the claim ages out
regardless. On a coord predating qontinui/qontinui-coord#2206 the heartbeat
never answered `not_held` (that was only the release door's word):
`HeartbeatResult` (`claims.rs`) was `ok` or `stolen`, so the discriminator
is `current_holder`, not the verdict word: a NAMED holder is a token mismatch or
a theft (drop the owner token and coord names *you*, since you are still the
stored owner), while `current_holder: null` is a claim that had already expired.
`scripts/coord-claim-heartbeat.sh` makes that split and records the null case
`lapsed`. Opposite recoveries - fix the owner token, versus re-`acquire` - so
read `current_holder`. That is a coord PREDATING qontinui/qontinui-coord#2206;
#2206 landed 2026-09-17 and is deployed, and a current coord answers the expired
case `not_held` here, with a named holder always on `stolen`. `hb_row` maps both
spellings.

Read the loop at any point with `status`, which prints one line per row plus a
verdict on its **exit code**: `LIVE` (0), `STALE` (3), `DEAD` (4), `STOLEN` (5),
`LAPSED` (7), `EMPTY` (8), `MAX_RUNTIME` (9 — the loop ended on its own
`--max-runtime` ceiling, not a coord verdict; re-`add` then `start`).

```bash
bash <workspace-root>/qontinui-claude-config/scripts/coord-claim-heartbeat.sh status --ledger "$CLAIM_LEDGER"
```

Anything other than `LIVE` means the reserve is **UNKNOWN, not held** — the same
`silent-empty-is-unknown` reading the fail-closed arm above applies to the
acquire. Say so in the Step 5 report rather than assuming the reserve survived.

#### Refresh the agent token — (a) after the reserve, (c) before every closeout write

A bare agent JWT expires on its own clock, and that clock is shorter than a long
chain: the reserve succeeds, hours pass, and the closeout writes 401 against a
token that was valid when the chain started.
`scripts/coord-agent-refresh.sh` renews it **in place** — it mints no new
identity. Run it, with no arguments, at exactly two points in this command:
**(a)** here, immediately after the reserve, and **(c)** immediately before each
of Step 5's closeout writes (the release, plus any gate attestation, work-unit
transition or finding that report carries).

```bash
bash <workspace-root>/qontinui-claude-config/scripts/coord-agent-refresh.sh
```

It prints exactly one verdict line. `PROXIED` (the runner refreshes for you),
`FRESH <exp> <ttl>` and `REFRESHED <new_exp>` all exit 0 and mean carry on.
`CREDENTIAL_ONLY <exp>` (exit 6), `EXPIRED <exp>` (exit 7) and
`REFUSED <status> <body>` (exit 8) each mean the next coord write will fail
authentication: report the verdict verbatim and follow the next step the helper
names. **Never** call `POST /agents/allocate` to work around one — that door is
prohibited as a credential rung, and the helper exists so nobody needs it.

Every coord bearer this command reads resolves **file first**
(`$HOME/.qontinui/agent-jwt/<AGENT_SESSION_ID>`), then `$COORD_AGENT_JWT` — the
file is what the helper rewrites, so an env-first read would keep sending the
stale token after a successful `REFRESHED`.

#### Release — the acquirer releases

Fire this at Step 5, in the same try/finally so it also runs on every abort path
(a `/vet-plan` abort at Step 3, an operator **Abort** above, a failed chain).
**Stop the heartbeat FIRST** — `remove` the row, then `stop` the loop — so the
loop cannot re-arm a key this chain is about to give up:

```bash
bash <workspace-root>/qontinui-claude-config/scripts/coord-claim-heartbeat.sh remove \
  --ledger "$CLAIM_LEDGER" --kind semantic_resource --key "plan:<plan-stem>"
bash <workspace-root>/qontinui-claude-config/scripts/coord-claim-heartbeat.sh stop --ledger "$CLAIM_LEDGER"
```

`remove` of the last row stops the loop by itself and `stop` is idempotent
**once the ledger is empty**, so running both in that order is safe. Both are
skipped on `renewed`, exactly as the release is — this chain neither started that
loop nor owns it.

⚠️ **`stop` REFUSES (exit 6) while rows remain — report it, do not `--force`
past it.** The ledger is keyed on `$CLAUDE_CODE_SESSION_ID`, which a subagent
**inherits**, so one file is shared by every context under one harness session
and this loop is the sole renewer of every row in it. Rows still present mean the
`remove` above did not run, or a sibling context still holds claims. Note the
`renewed` skip does **not** cover the subagent case: a subagent running this
chain on its own plan reserves a key the parent never took and gets `granted`
(measured 2026-09-04). Plan
`2026-09-04-closeout-commands-have-no-subagent-arm-and-finish-their-parent`.

Then release:

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
token is the match key, so omitting it returns `"not_held"` and leaves the
reservation to TTL out. **Skip the release entirely if this step returned
`renewed`** — something outside this chain acquired the reserve and owns it.

⚠️ **A `not_held` release is evidence the lock LAPSED. It is not a no-op.** The
route is idempotent, so nothing downstream breaks — which is exactly why this
answer used to be waved through as "fine". It is not fine. With the owner token
sent correctly (above), there are only two ways to reach it: the reserve expired
because nothing heartbeated it, or another session stole it. Either way this plan
was **unprotected for some part of the chain**, which is the window a duplicate
PR pair is born in. So:

- **Report it** in the Step 5 report as a lapse, naming the plan key — never as
  a clean idempotent release.
- **Say SINCE WHEN.** Read the ledger:
  `bash <workspace-root>/qontinui-claude-config/scripts/coord-claim-heartbeat.sh status --ledger "$CLAIM_LEDGER"`. Each
  row's last-ok timestamp is the last moment the reserve is *known* to have been
  held, so the lapse window is from there to now; a `STOLEN` verdict names a
  theft rather than an expiry, `LAPSED` says a row's grant is gone (an expired
  answer, aged past its TTL, never confirmed, or a malformed TTL), `EMPTY` says the loop
  lived but the ledger held no row to renew (an `add` was refused or never ran),
  and `DEAD` says the loop was not running at all. If the ledger itself is
  missing, the window is UNKNOWN — say that, rather than reporting a number you
  do not have.
- **Recover before any further work that needs the reserve.** Re-acquire it,
  re-`add` it with `--ttl` set to the new response's `ttl_seconds`, then run
  `start` (idempotent, and required whenever the loop has ended — `DEAD`, or
  `STOLEN` with no live pid), then re-read `status` and require `LIVE`.

### Step 1.5 — Provisionally label the session with the plan slug

At the start of the run no PRs exist yet, so label the session with the plan
slug as a provisional identifier. Step 5 replaces this with the final
PR-numbered name from `/name` once implementation has opened the PRs.

Derive a session label from the resolved plan filename — the basename with the
folders, the leading `YYYY-MM-DD-` date prefix, and the `.md` extension stripped
(e.g. `<plans-dir>\2026-06-18-coord-cancelled-ci-not-main-red.md` →
`coord-cancelled-ci-not-main-red`; the backslashes are deliberate — the snippet
below normalizes Windows separators) — and set it as this session's title.

The built-in `/rename` command can't be invoked from inside a slash command, so
write the same record `/rename` writes: a `custom-title` entry appended to the
current session's transcript JSONL. Run this once, substituting the resolved
absolute plan path for `<PLAN>`:

```bash
PLAN="<PLAN>"
plan_norm="${PLAN//\\//}"                       # normalize Windows backslashes
slug="$(basename "$plan_norm" .md)"
slug="${slug#[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]-}"   # drop YYYY-MM-DD- prefix if present

# Append the same entry /rename writes, to THIS session's transcript.
# Best-effort only — never block the vet → implement chain if it can't run.
if [ -n "$CLAUDE_CODE_SESSION_ID" ]; then
  sf="$(find "$HOME/.claude/projects" -name "$CLAUDE_CODE_SESSION_ID.jsonl" 2>/dev/null | head -1)"
  if [ -n "$sf" ]; then
    uuid="$(python -c 'import uuid;print(uuid.uuid4())' 2>/dev/null || echo "00000000-0000-4000-8000-000000000000")"
    ts="$(date -u +%Y-%m-%dT%H:%M:%S.000Z)"
    printf '{"type":"custom-title","customTitle":"%s","sessionId":"%s","uuid":"%s","timestamp":"%s"}\n' \
      "$slug" "$CLAUDE_CODE_SESSION_ID" "$uuid" "$ts" >> "$sf"
    echo "session labeled: $slug"
  fi
fi
```

This sets the **persisted** title — it shows immediately in the session list and
the resume picker. It does **not** live-refresh the title bar of the already
running session (that value lives in process memory and only the interactive
`/rename` updates it); the label takes visible effect on the next render of the
session list / on resume. If a live title-bar update is wanted this run, mention
the derived slug in the Step 5 report so the operator can paste `/rename <slug>`.

This step is best-effort: if `$CLAUDE_CODE_SESSION_ID` is unset or the transcript
file isn't found, skip silently and proceed — never let labeling block vetting.

### Step 2 — Vet the plan

Invoke `/vet-plan` via the **Skill tool**, passing the resolved plan path as the
argument:

```
Skill: vet-plan
Args: <resolved plan path>
```

Let `/vet-plan` run to completion — it audits the claims, edits the plan in
place, resolves open questions via its Decision policy, and stamps the plan
`Status: VETTED <date>` (and, under this chain, registers the `time_elapsed`
safety-net gate — **not** a `unit_ready` record gate; see the table below). Do
not short-circuit any of it.

**Collect its report; do not emit it.** `/vet-plan` Step 6 produces a complete,
standalone-looking report. Under this chain that report is an **intermediate
result**: hold it, and fold it into the single combined report at Step 5. Do NOT
render it as a finished deliverable here, and do NOT let it end the turn —
`/vet-plan` Step 6 has the matching instruction on its side.

The reason is mechanical, not stylistic. A finished-looking deliverable at the
midpoint is a **stop cue**: it reads as "the job is done", and the chain has been
observed to end right there with Steps 3-5 never reached (diagnosed 2026-07-28,
reproduced live). Nothing report-shaped may exist between the VETTED stamp and
the `Skill: implement-plan` call — removing that mid-chain terminal beat is the
point, so do not reintroduce it as a "quick summary of the vet" either.
(`/implement-plan` still emits its own report at the end of its run; the rule is
about the MIDPOINT, not a cap on output.)

**`/vet-plan` registers exactly ONE gate for a `/vet-imp` run — the net
(changed 2026-09-04).** If this chain drops after vetting, coord dispatches a
fresh visible session to implement the plan instead of leaving it stranded.

| Gate | `phase_name` | Predicate | Continuation | Registered under `/vet-imp`? |
|---|---|---|---|---|
| **Record** | the plan title, or `"vet→implement handoff"` | `unit_ready` `{work_unit_id, ready_status}` | **NEVER** — unconditional | **NO — standalone `/vet-plan` only** |
| **Net** | `"vet→implement safety net"` | `time_elapsed` `{duration_secs: 1800}` | the dispatching `continuation_spawn`, **carrying a brief** | **YES — always** |

> **The net's continuation carries a brief; `/vet-plan` §5.4 shows the shape in
> step 5 and step 6 registers it.** The session coord dispatches has never read the plan and does
> not know this chain existed: without a brief its whole context is
> `run /implement-plan <path>`. This is the untagged `continuation_spawn` shape,
> which has no `hint` field, so the brief is appended to `initial_prompt` after
> a blank line — byte-identical to what coord builds from a typed `hint`.
> Canonical: `_gate-registration` → "The brief — a continuation with no `hint`
> is a fresh agent with no context" — keep copies in sync.

> ⚠️ **Why the record gate is not registered under this chain.** Its predicate
> holds only while the unit is at its vetted status with no unmuted sibling
> open. The net above is registered on the same `work_unit_id` seconds later
> and pins it `Open`; `/implement-plan` Step 0.5 then transitions the unit to
> `in_progress` **before** it mutes that net — so the window in which the record
> gate could clear is closed by this chain's own next step, and the gate fails
> **OPEN** with no alert until the 7-day stale sweep. Measured 2026-09-04 over
> 26 work units: **6 of 25 `unit_ready` gates (24%) ended unclearable**; the rest
> survived only by winning a several-second race against the sweep. Under
> `/vet-imp` the gate advertises dispatchable work to a chain already doing it,
> so it has no consumer — dropping it removes the coin flip and costs nothing.
> Plan: `2026-09-04-vet-imp-leaks-an-unclearable-unit-ready-gate-every-run`.
>
> ⚠️ **Withdrawing a stranded record gate no longer FORECLOSES readiness — but
> it does not PRODUCE it either (verified 2026-09-18 against the serving
> build).** This block used to forbid withdrawal, and was right to: until
> qontinui-coord **`47014372`** (*"fix: stop a retired gate permanently barring
> its work unit from `ready`"*, an ancestor of the serving build `b826916e`) a
> `withdrawn` row stayed in `total` forever, so the unit could never reach
> `total == cleared` again even with a fresh cleared gate. That is gone.
>
> ⛔ **But `all_unit_gates_cleared` is `total > 0 && total == cleared`, and the
> first conjunct matters.** The query now excludes archived, muted AND withdrawn
> rows (`work_unit_derive_worker.rs:752` — the old `:337-353` citation had
> drifted), so withdrawal REMOVES the row: withdraw the last counted gate and
> `total = 0`, which is **vacuously false**. Coord's own regression test only
> demonstrates the fix on a unit that keeps another cleared gate. So withdrawal
> is **cleanup, not repair** — it un-pins the unit; reaching `ready` still needs
> a counted gate driven to `cleared`. A unit that retains one promotes itself on
> the next derive tick with zero writes.
>
> Mute and withdrawal are now indistinguishable to this predicate, so neither
> helps a unit reach `ready` — the record gate must stay counted AND clear.
> `failed` / `misconfigured` deliberately still block.
>
> ⚠️ Verified for the **ready-derivation path only**; other predicates are
> UNVERIFIED. Record the `gate_id` either way. Canonical: `_gate-registration`.

> ⚠️ **Do not "simplify" this back into one gate with a continuation on
> `unit_ready`. That configuration cannot work, and coord now refuses it at the
> door.** `/vet-plan` §5.4 mandates transitioning the unit to its vetted status
> *before* registering the record gate keyed on the status that landed, and
> `ready_verdict` is a bare `status != ready_status` compare — so
> `status == ready_status` **by construction** and a freshly-upserted unit has no
> open siblings. `Cleared` is the only reachable verdict from the first
> evaluation onward, and the 10 s `run_gate_sweep` clears it and consumes the
> continuation within ONE tick. Measured 2026-08-26 (gate `87e8e72b`):
> dispatched, `consumed_outcome: "spawned"` 509 ms later, while
> `/implement-plan`'s cancel arrived minutes afterwards to a
> `409 already_consumed` — a redundant terminal on **every completed run**, not a
> residual. Coord now answers such a registration with the
> `continuation_dropped_born_cleared` steer: the gate is created, the
> continuation is silently **not armed**, and the chain has no net at all.
>
> Reproduced live 2026-09-02 in a `/vet-imp` run on a machine whose
> `.claude` checkout was **45 commits behind `origin/main`** and so loaded the
> pre-2026-08-30 `/vet-plan`: the record gate came back
> `continuation_dropped_born_cleared`, and the run finished with no net armed.
> Staleness is the delivery risk here — this fix cannot help a session reading an
> old checkout, so `git -C <workspace>/qontinui-claude-config status` is worth a
> glance when a documented mechanism does not behave as written.

The net's window is genuinely unsatisfied for its whole 30 minutes, which is the
property `unit_ready` could not provide: it is false the instant it is armed and
becomes true only if nobody picks the plan up.

`/implement-plan` Step 0.5 retires the net when it stamps IN PROGRESS —
**cancel, then mute**, on the `"vet→implement safety net"` gate specifically
(`coord_cancel_continuation {gate_id, reason}`, or the REST twin
`POST $COORD_HTTP_URL/coord/gates/<gate_id>/agent/continuation-cancel`; then
mute, or the record gate stays pinned `Open` on it as a sibling —
`coord_withdraw_gate` is the one-call equivalent and is LIVE). At that stamp the
**expected** row state is `continuation_spawn != null ∧ dispatched_at == null`
— pre-dispatch and armed, which is precisely what a 30-minute window exists to
produce, and `cancel_continuation` deliberately omits the
`continuation_dispatched_at IS NOT NULL` guard (*"the pre-dispatch stamp is the
whole point"*). A `409 already_consumed` now means the chain took **longer than
the window** to reach Step 0.5, not that the race is unwinnable; the residual is
then a **visible** redundant terminal that should stand down at
`/implement-plan` Step 0.45 / Step 0.6 — never a silent strand.

This is a **backstop, not a licence to stop here**: a stalled chain that gets
rescued by coord still burns a session and delays the work.

### Step 3 — Gate: confirm the plan is actually VETTED

After `/vet-plan` returns, re-read the top of the plan file and confirm its
status block now reads `Status: VETTED`. This gate exists because the two
states that legitimately stop the lifecycle must stop it here too:

- **`/vet-plan` aborted** because the existing block was `SHIPPED` /
  `SUPERSEDED` / `OBSOLETE` (its Step 0.25 stops on a closed plan BEFORE editing
  it — the §5 paragraph that also states the rule runs too late to refuse
  anything), because the
  block read `IN PROGRESS` and its conditional guard refused (the work has
  landed, or a live peer holds it — see `/vet-plan`'s
  "`IN PROGRESS` is CONDITIONALLY overwritable"), because its own §0.25
  delivery read landed on
  **arm 1** — `shipped: true` ∧ `evidence_complete: true` with the phase axis
  corroborating it, the arm all three readers carry — and routed to closeout
  instead of vetting, or because it judged the plan's **overall
  architectural direction wrong** and surfaced that instead of editing. In any of
  those cases do NOT proceed to implement — relay the vet skill's reason to the
  user and stop.
- **The work has ALREADY LANDED.** Independently of the stamp, read the derived
  delivery before proceeding:
  `coord_work_unit_list_citations(<plan-stem>) -> .delivery`. **STOP** and
  route to closeout — do not implement — only when EVERY one of these seven
  holds:

  1. `shipped: true`
  2. `evidence_complete: true`
  3. no top-level `merged_degraded_reason`
  4. `phases_declared` is **not `null`**
  5. `phases_remaining == []` — the LITERAL empty list, never `null`
  6. that `[]` is **corroborated by the same response** (the corroboration rule
     below)
  7. `evidence_gaps` carries no `NO PHASE ATTRIBUTION` entry

  Short of all seven it is a fall-through, not a stop. Read the response
  through `/vet-plan`'s arm table in its stated order — **4, 3, 2, 1, 5, then
  6** — with these seven as arm 1 here (a NON-EMPTY `phases_remaining` beside
  `shipped: true` is `/vet-plan`'s one non-inconclusive arm-6 sub-case: the
  implement case below), so the UNKNOWN arms 3 and 2 are ruled out
  before the closeout STOP of arm 1 is taken. An `IN PROGRESS` stamp is not
  re-decided here: `/vet-plan` Step 0.25 already applied its
  "`IN PROGRESS` is CONDITIONALLY overwritable" section, including its
  **unidentified default**
  (an `IN PROGRESS` stamp with no session marker, or one that cannot be
  positively attributed at all, is a STOP, not an overwrite; a marker naming a
  probed-dead peer is case 2, adopt), before it rewrote the stamp.

  **Conjunct 3 is carried explicitly even though that arm order already
  enforces it** — `/vet-plan`'s arm 3 pre-empts arm 1 on a
  `merged_degraded_reason` *"whatever `delivery` says"* — so that the seven
  read correctly on their own, without the arm table beside them. On today's
  code it is redundant with conjunct 2 (the degraded probe pushes its gap before
  `evidence_complete = evidence_gaps.is_empty()` is computed, from the same
  `merged_predicate_degraded_by` that fills the envelope field), but `/vet-plan`'s
  own arm-3 text asserts the DECOUPLED reading — the field is *"present even when
  the verdict could not be derived at all"* — and on that reading a reader
  applying the other six without the arm table would STOP where `/vet-plan` answers
  UNKNOWN. Naming it keeps the list safe to read on its own.

  `phases_remaining: null` is UNKNOWN on
  the phase axis, DISTINCT from `[]`: it falls through to the stamp arms, and you
  say so plainly in the Step 5 report. Test
  `null` BEFORE you test emptiness — a truthiness or emptiness test reads `null`
  as falsy-empty and reproduces the exact false STOP plan
  `2026-09-18-coord-fabricates-a-phase-declaration-and-silences-its-own-gap`
  exists to prevent, so do not "simplify" the three-way read back to two.

  Treat `evidence_complete: false`, or a top-level
  `merged_degraded_reason`, as **UNKNOWN rather than "undelivered"**, and a
  `no work-unit with that slug` error as *not-found* (say so; proceed).
  **Everything else is UNKNOWN too — that is the DEFAULT, not a gap.** Any other
  error (coord's `citation surface unavailable for work-unit …`, whose own text
  says it is NOT "this unit has no citations"; its generic
  `citation list failed: …`), any unparseable or non-2xx body, a
  `citations_error` / `delivery_error` key, an absent `delivery`, or the tool
  masked / absent / on a dead transport (`"Command failed with no output"`) must
  never be read as "not delivered". **On any UNKNOWN, do not treat the delivery
  read as evidence in either direction**: run **`/coord-revive`** if the
  transport is dead and re-issue over the live door; if it still cannot be
  answered, say so plainly in the Step 5 report and let the stamp arms decide.
  Never let an unanswerable read silently license implementing. This mirrors
  `/vet-plan`'s arm 6 — keep the two in sync — and the STOP rule above must stay
  in sync with `/vet-plan` §0.25 arm 1 and `/implement-plan` Step 0.5's
  `IN PROGRESS` rule, which are the same contract read twice more.

  > ⚠️ **The corroboration rule — what entitles a literal `[]` to stop.** The
  > literal `[]` may stop **only when the same response corroborates it**:
  > `phases_declared_indices` is non-empty and every index in it also appears in
  > `phases_delivered`. When `phases_declared_indices` is non-empty and
  > `phases_delivered` does not cover it, `[]` contradicts its own neighbours —
  > coord is claiming nothing remains while not having attributed every declared
  > phase — so read
  > it as UNKNOWN and fall through. An EMPTY `phases_declared_indices` is not
  > corroboration either: there is nothing for the `[]` to be the answer to, so
  > that is UNKNOWN as well — and so is a `null` one, which is how a coord build
  > carrying the coord-fix plan's Phase 3 spells a declaration it could not
  > establish (the coord-fix plan is `2026-09-18-coord-fabricates-a-phase-declaration-and-silences-its-own-gap`
  > throughout this arm — not the plan you are running).
  >
  > **This test is deliberately BUILD-INDEPENDENT, and that is why it is the
  > test.** `phases_declared_indices` and `phases_delivered` are already served
  > beside `phases_remaining` by the coord build running today, so the rule needs
  > no deployment tell — no version probe, no "has the nullable contract reached
  > this tenant yet" question you would have to answer before you could read the
  > field at all. It returns the same verdict on a PRE-contract coord (which
  > spells every non-established state `[]`) and on a post-contract one (which
  > spells them `null`), because on both it is the neighbours that decide.
  > Verified against the measured shape: a live read of the work unit for plan
  > `2026-09-18-coord-fabricates-a-phase-declaration-and-silences-its-own-gap`
  > on 2026-09-18 returned `phases_declared: 5`,
  > `phases_declared_indices: [1,2,3,4,5]`, `phases_delivered: []` and
  > `phases_remaining: []` — declared indices non-empty, delivered covering none
  > of them, so the `[]` is UNCORROBORATED and there is **no stop**. That is the
  > correct outcome on either build. **What that example does and does not
  > show:** the same read also returned `shipped: false`, so conjunct 1 had
  > already ruled out a stop — it shows how the fields look, not that
  > corroboration decided it. (On a build carrying the coord-fix plan's Phase 3
  > the same unit reads `phases_remaining: null`, not `[]`; the verdict is the
  > same, the spelling is not.)
  >
  > **What the rule costs — stated so you do not "fix" it.** A genuinely
  > complete unit whose completeness was asserted some OTHER way fails
  > corroboration and falls through to the stamp arms instead of stopping.
  > **Two spellings, and the SECOND is the larger population today.** One: a
  > citation declaring `complete`, which satisfies coord's coverage gate for a
  > plan of any length while attributing no index at all, so `phases_delivered`
  > stays empty. Two: landed citations carrying **no phase scope at all**, which
  > leaves `phases_delivered` empty the same way. For a declaration of two or
  > more phases that second spelling costs nothing new — coord raises the
  > `NO PHASE ATTRIBUTION` gap there and the gap conjunct already blocked the
  > stop. It bites on a **single-phase declaration**, where that gap is
  > withheld on the build serving today (coord's Gap-5 arm requires
  > `d.len() > 1` until the coord-fix plan's Phase 2) and the stop therefore
  > used to fire: that is likely a large share of genuinely-complete one-phase plans on today's
  > fleet, not an edge case. It is a FALSE NEGATIVE, and it is **not free** —
  > say what actually catches it. The stamp arms do not read `origin/main`: in
  > `/vet-plan` they read the status block captured from disk at §0.25 item 1,
  > and here the stamp is the `VETTED` this chain's own Step 2 just wrote. A
  > `SHIPPED`, `SUPERSEDED` or `OBSOLETE` stamp, or a foreign `IN PROGRESS`
  > whose session left PRs, still stops at `/vet-plan`'s pre-edit capture; a
  > complete plan still stamped `VETTED` or `DRAFT` (or `PARTIAL` / `NOT
  > STARTED`) is re-vetted and re-implemented, and the only remaining MECHANICAL
  > guard is `/implement-plan` Step 0.45 check 2 — a `git log origin/main -20`
  > grep that misses anything older — though `/vet-plan` §2's claim verification
  > may also notice the plan's premises are already met. That cost
  > is accepted because a false STOP is worse — it abandons live work on a read
  > where every field looked healthy, with no guard after it at all. Do not trade
  > that protection away to buy back the false negative.

  > ⚠️ **`shipped: true` with a NON-EMPTY `phases_remaining` is NOT a stop — it
  > is the single most valuable row this read can return.** coord is saying: at
  > least one cited PR landed, and these declared phases have no landed
  > evidence. IMPLEMENT THOSE PHASES. *(Plan
  > `2026-09-13-coord-delivery-cannot-express-partial-delivery`, Phases 3 and 5.)*
  >
  > `shipped` has never meant *"all phases done"* — it is *"≥1 cited PR landed ∧
  > none blocking"*, narrowed by coord's phase gate only when a landed citation
  > carries a phase attribution on a declaration of two or more phases — so a
  > genuine Phase-1 PR carrying no phase scope on a six-phase plan derives it and
  > the remaining five are never dispatched again. Measured 2026-09-15: **295
  > units** read `shipped` against a non-terminal `origin/main` stamp, **277**
  > with stated or structural residue. `phases_remaining` is the field that makes
  > that condition readable instead of inferrable from prose.
  >
  > **An empty `phases_remaining` is not proof of completeness, and `null` is
  > not empty.** The field is empty in four different situations and only one of
  > them is "everything landed": the unit declares no phase list, a declaration
  > exists but coord could not parse it, **no landed citation carries a phase
  > attribution at all**, or coverage really is total. That fourth situation has
  > two spellings and both count as total — every declared index carries a
  > landed attribution, or **a single landed citation declared `complete`**,
  > which satisfies coord's coverage gate whatever the declared count is. The
  > corroboration rule above is what separates them: only the first spelling can
  > corroborate itself, and the `complete` spelling deliberately falls through
  > (see "What the rule costs"). Under the nullable contract *(plan
  > `2026-09-18-coord-fabricates-a-phase-declaration-and-silences-its-own-gap`,
  > Phase 3)* the first three render **`null`** — coord could not establish the
  > phase axis — and only the fourth renders **`[]`**: coord looked and nothing
  > remains.
  >
  > **Two fields, not one, and the names are close enough to mislead.**
  > `phases_declared` is a **count** (`Option<usize>` — `null` exactly when coord
  > never knew the plan's phase shape). The declared INDICES are a separate
  > field, `phases_declared_indices`, and `phases_delivered` is the attributed
  > union. So wherever this page says `phases_declared: null`, it is the COUNT that is null — do
  > not go looking for a list under that name.
  >
  > The third situation is the common case today, and coord discloses it —
  > `evidence_gaps` then carries a `NO PHASE ATTRIBUTION` entry. On the build
  > serving today that disclosure is **withheld when the declaration has length
  > 1**; the coord-fix plan's Phase 2 makes it unconditional, so do not read the
  > conjunct as already-guaranteed cover. The SECOND situation — a declaration
  > coord could not parse — has **no named gap on the build serving today**; a
  > build carrying the coord-fix plan's Phase 5 names it (`UNREADABLE PHASE
  > DECLARATION`, and `NO PHASE DECLARATION` for the first situation). The rule
  > never keys on either name, so it gives the same verdict whichever build
  > answers — do not add a conjunct that tests for them.
  >
  > On the serving build that second situation reads `phases_declared: null`
  > beside `phases_remaining: []`; after the coord-fix plan's Phase 3 it reads
  > `phases_remaining: null` and the `null` test catches it too. Conjunct 4
  > (`phases_declared` is not `null`) is **implied by conjunct 6 on both
  > builds**: a null count comes with an EMPTY `phases_declared_indices` today
  > (coord renders the indices with `.unwrap_or_default()` off the same
  > `Option`) and a `null` one after Phase 3, and corroboration refuses both. It
  > is kept as a NAMED restatement of the state, not because anything depends on
  > it alone; `/vet-plan`'s arm 1 omits it for exactly that subsumption reason.
  >
  > What neither catches is a declaration coord MIS-parsed through the
  > positional fallback, so `1..=len` is fabricated. **On the build serving
  > today** that is any list whose every index is UNREADABLE, which is not the
  > same as unnumbered: the key may be absent, or PRESENT and rejected
  > (`from_metadata`'s `>= 1` filter rejects `0`, and a string, a negative or an
  > overflow is rejected too) — so do not rule the fallback out just because you
  > can see an `index` key. For a single-entry list (the `cf9ae0b2` shape) it
  > renders a non-null `phases_declared: 1` / `phases_declared_indices: [1]`
  > and only corroboration catches it; for two or more entries the
  > `NO PHASE ATTRIBUTION` gap also fires when nothing is attributed. **On a build carrying the coord-fix plan's
  > Phases 1, 3 and 5**, index 0 is read as `0` and a present-but-rejected index
  > is UNKNOWN (rendered `null`), so the fallback survives only for a list with
  > NO `index` key at all — and that build names it:
  > `phases_declaration_provenance: "positional"`.
  >
  > **Check the GAP LIST itself, never the `evidence_complete` flag.**
  > `evidence_complete` is computed BEFORE the phase gap is appended
  > (`evidence_complete = evidence_gaps.is_empty()`, and only then is the gap
  > pushed), deliberately, so that a phase gap discloses without dropping the
  > flag. `evidence_complete: true` beside a NON-EMPTY `evidence_gaps` is
  > therefore reachable BY DESIGN — and both the identity `/vet-plan` states and
  > the serving build's coord docstring (*"Empty iff `evidence_complete`"*;
  > a build carrying the coord-fix plan says "NOT 'empty iff'") describe the
  > pre-push moment, not the object in front of you. A reader who trusts either
  > one will "simplify away" the gap conjunct. Read the list.
  >
  > Why the gap tell alone was not enough, and why `null` had to be distinct: on
  > unit `cf9ae0b2` a single-entry `[{"index":0}]` declaration was parsed as a
  > phantom `{1}`, the `len == 1` exemption silenced the `NO PHASE
  > ATTRIBUTION` gap, and `phases_remaining: []` rendered *"nothing remains"* on
  > a plan with four phases outstanding — every field of this arm read healthy
  > and it STOPped a live chain. A defence keyed on a gap that the defect itself
  > suppresses is not a defence; that is why the stop now turns on the
  > corroboration rule, which the defect cannot fabricate **from silence**.
  >
  > **Bound that claim there, because corroboration is not proof.** It tests
  > coord's own numbers against each other, not the declaration against the
  > plan, so a declaration that is wrong *and* covered still corroborates.
  > Constructible today: the same phantom `{1}` plus one landed citation whose
  > marker carries `phases: 1` gives `phases_declared_indices: [1]` ⊆
  > `phases_delivered: [1]`, `phases_remaining: []` and no gap — **corroborated,
  > and it STOPs**, on a plan with phases outstanding. Same shape for any
  > thinly-parsed multi-phase plan whose single landed PR cites the one index
  > that was parsed. It is rare today only because phase attribution is rare,
  > and the direction of travel is to make attribution common. That residue is
  > not closed by any single phase, and not by this arm. The coord-fix plan's
  > **Phase 1** closes only the present-but-rejected-index arm of the positional
  > fallback (`[{"index":0}]` then reads `[0]`, and an unreadable index reads
  > `null`). A thin but CORRECTLY numbered parse — the runner writing one entry
  > for a five-phase plan, split to plan
  > `2026-09-19-runner-detect-phases-misses-plans-that-list-phases-in-a-table-or-prose`
  > — corroborates the same way, and coord cannot detect it at all: after Phase 1
  > a `{0}` declaration beside one landed `phases: 0` citation STOPs just as the
  > phantom `{1}` did.
  >
  > One last consequence for how you READ the field: never collapse `null` and
  > `[]` when serialising the read. A `jq` `// []` default or a Python `or []`
  > does exactly that, and re-arms the false STOP.

  > **This arm exists because Step 3 cannot catch the class on its own.** Step 3
  > re-reads a stamp that **this chain's own Step 2 just wrote**, so the gate is
  > self-satisfying by construction: whatever `/vet-plan` stamped, Step 3 confirms.
  > Worse, the overwrite is not merely a missed guard — it **destroys the evidence
  > the downstream guard reads**. `/implement-plan` Step 0.45 check 1 surfaces a
  > foreign `IN PROGRESS` stamp to the operator, so a standalone
  > `/implement-plan` would at least have paused there; under `/vet-imp` it
  > cannot, because the stamp is `VETTED` by the time Step 0.45 looks. Do not
  > over-credit that check even when it does run: it offers a **Proceed anyway**
  > option rather than stopping. The delivery read is the only signal in this
  > chain that no earlier step can overwrite.

- **The stamp is missing** for any other reason. Do not implement an unvetted
  plan; report what `/vet-plan` actually produced and stop.

If the status block reads `VETTED`, proceed to Step 4. A defect count > 0 in the
VETTED summary is **not** a blocker — `/vet-plan` auto-fixes what it can and only
surfaces genuine product/scope calls; those are reported, not gating.

**Confirming VETTED and invoking `/implement-plan` happen in the SAME assistant
turn, with the Skill call last.** Do not confirm the gate in one turn and plan to
invoke in the next — there is no next turn; the turn ends and the chain is dead.
If you have just written the words that confirm the stamp, the very next thing
you emit is the Step 4 Skill call, not a summary and not a hand-off sentence.

### Step 4 — Implement the vetted plan

Invoke `/implement-plan` via the **Skill tool**, passing the same resolved plan
path (plus any forwarded implement-only flags from `$ARGUMENTS`):

```
Skill: implement-plan
Args: <resolved plan path> [forwarded flags]
```

`/implement-plan` takes over from VETTED: it runs its own dependency gate,
concurrent-work reconnaissance, claim pre-flight, the IN PROGRESS stamp, the
phase agents, manual testing, commit, and the SHIPPED stamp. Because
Step 2 just stamped the plan VETTED in this same session, `/implement-plan`'s
Step 0.5 will see a fresh VETTED block and start cleanly — it will NOT warn that
the plan was never vetted.

### Step 5 — Final session name + report

First, derive the **final session name** by invoking the `/name` command via the
**Skill tool** (no arguments — let it auto-detect):

```
Skill: name
```

`/name` detects the open PRs this run just opened and returns a name of the form
`<pr-numbers> <descriptive words>` (e.g. `614,615 coord gate robustness`),
emitted as a ready-to-run `/rename <name>` line. This supersedes the provisional
plan-slug label from Step 1.5 — now that implementation has opened PRs, the
PR-numbered name is the better identifier.

Persist that name as the session title using the **same transcript-append
mechanic as Step 1.5** (substitute the name `/name` produced for `$slug` in that
bash block). Same best-effort rules apply: if `$CLAUDE_CODE_SESSION_ID` or the
transcript file is missing, skip silently. If `/name` finds zero open PRs (e.g.
the run deferred to a gate without opening a PR), fall back to the Step 1.5 plan
slug.

**Before writing a word of the summary, compute the chain's verdict.** When the
`/implement-plan` half opened PRs it also took the pre-PR-review arm for them
and recorded which one at `~/.qontinui/review-arm/<session-id>.json`, keyed on
the `AGENT_SESSION_ID` resolved once in Step 1.1 —
`${QONTINUI_AGENT_SESSION_ID:-${CLAUDE_CODE_SESSION_ID:-}}`, the same value in
both halves of the same harness session, which is the whole reason this half can
read back what the other one wrote. Look for it under that id:

```bash
ARM_FILE="$HOME/.qontinui/review-arm/${AGENT_SESSION_ID}.json"
```

**PRs opened and no `review-arm.json` under that id ⇒ the chain is INCOMPLETE,
and this command refuses to report it complete.** Open the summary with that
verdict instead of burying it: name the artifact path you looked for, the
session id you looked under, and the PRs that are therefore uncovered. An empty
`AGENT_SESSION_ID` is the same verdict and not an excuse — the implement half
writes nothing without one, so there is nothing to read back; say the id was
empty. An artifact that exists but covers none of the PRs this run opened is
INCOMPLETE for the ones it omits, and saying so is the point: an arm nobody can
name is the failure this gate exists for.

This is a **presence check on the hand-off, not a review**. Do not read the
registry, do not select an arm, do not write the artifact and do not review the
diff from here — `/implement-plan` owns all four, and a second copy of that
branch in this file is a second thing to diverge. Step 5 asserts only that the
artifact the other half was required to produce is there.

**Then corroborate it against the one signal the implementer did not write**
*(plan `2026-09-03-every-verification-signal-is-written-by-the-implementer`
Phase 3)*. Presence proves the review gate ran; it cannot prove the commit it
reviewed is the commit on the PR. coord's status card can, and it was written by
no session. Run the same script `/implement-plan` Step 4.7 runs — it is one
implementation, invoked from two places, not a second copy of the check:

```bash
bash <workspace-root>/qontinui-claude-config/scripts/review-arm-corroborate.sh --artifact "$ARM_FILE" --tree-root <the worktree the implement half reviewed>
```

**Run it in the background.** Door calls and tree re-measures are serial, each bounded by `REVIEW_ARM_CORROBORATE_TIMEOUT` (default 600 s), so a run can take up to (N+1+T) × that budget (N PRs, the coverage call, T checkouts re-measured). Start it with the Bash tool's `run_in_background` (or watch it with Monitor) and read the verdict line when it exits — a foreground call under the tool's 120 s / 600 s timeout is killed with no verdict.

Branch on its exit code, exactly as Step 4.7 documents it:

- **`0` CORROBORATED** — every PR's coord `head_sha` equals the artifact's
  `reviewed_head_sha`, the re-measured tree equals the artifact's
  `reviewed_tree`, and the coverage read found no PR coord attributes to
  this session that `prs[]` omits. Say so, per PR — **and quote the
  `coverage=` half of the verdict line as well.** The coverage read needs a
  session-scoped identity that a device-JWT session does not hold, so it
  routinely reads `coverage=UNKNOWN` on this fleet; that is reported beside the
  head verdict, never folded into it, and never written up as "coverage checked".
- **`2` CONTRADICTION** — head drift (`contradiction_head_drift`), a **working
  tree that moved since the review measured it** (`contradiction_tree_moved` —
  the arm a commit-only identity is blind to, because an uncommitted edit moves
  no sha at all), or an uncovered PR. **The chain is
  INCOMPLETE**, the same verdict as a missing artifact, and for the same reason:
  a review whose subject is not the code on the PR is a review nobody can name.
  Quote the `corroboration[]` / `coverage` row that failed. A head-drift
  CONTRADICTION also
  means the PR body's `Coord-Reviewed-Head:` line is stale — coord's
  `require_review` gate reads that line, not the artifact — so the re-review
  must edit the body (`gh pr edit <n> --body-file <file>`, `/implement-plan`
  Step 4.5) as well as re-record.
- **`3` UNKNOWN** — the door did not answer (`unknown_door` — it refused;
  `unknown_door_timeout` — its curl gave up with exit 28, its connect bound or
  its `COORD_REVIVE_CALL_TIMEOUT` total bound; `unknown_budget_expired` — this
  script's `REVIEW_ARM_CORROBORATE_TIMEOUT` expired before the door exited; the
  row's detail names the budgets involved and the elapsed time), the card reads `confidence:
  unknown`, the artifact predates `reviewed_head_sha`
  (`unknown_no_reviewed_head`) or `reviewed_tree` (`unknown_no_reviewed_tree`),
  or the tree could not be compared at all (`unknown_tree`: the producer
  refused, a field carries the unresolved sentinel, or the two lines name
  different checkouts — two unresolved measurements are string-equal and must
  never compare SAME). Report *"corroboration
  UNKNOWN"* with the row's detail, never "corroborated"
  [policy: `verification-and-evidence` `silent-empty-is-unknown`,
  `unknown-must-not-render-as-a-default`]. UNKNOWN does not by itself make the
  chain INCOMPLETE — it makes the report say what could not be established.
- **`4` USAGE** — no artifact: the presence verdict above already covers this.
- **Any other exit** (1, 129/130/143, …) — UNKNOWN: the run did not complete and
  the artifact may or may not have been rewritten. Report it as corroboration UNKNOWN, never corroborated.

The rows the script appends (`corroboration[]`, `coverage`) are merged into the
same artifact, so a later reader finds the implementer-written half and the
coord-observed half in one document and can tell them apart by `source`.

**The refusal changes the REPORTED STATUS ONLY.** It is a verdict computed here,
never an early return out of Step 5: everything below it still runs. It must not
suppress the `/name` call above it — labeling is best-effort and non-gating by
this file's own rule — and it must never skip the reserve release below. A
refusal that returned early would strand the Step 1.1 reserve for its full TTL
and hand the operator "I cannot vet my own plan", a worse failure than the one
it was reporting. **INCOMPLETE *and* released** is the correct end state.

Then give one short summary tying the two halves together: what the plan was
about, the vet outcome (defects found / auto-fixed / surfaced), and the
implement outcome (phases shipped, commit SHAs, anything deferred to a gate).
**Fold in the vet report you collected at Step 2** — it was never emitted, so
this is its only appearance; dropping it loses the vet outcome entirely. For the
implement half, defer to `/implement-plan`'s own report rather than repeating it
verbatim.

⚠️ **When reporting that PRs shipped/landed, don't take `gh pr checks` alone
as proof of landability.** `gh pr checks` enumerates checks that EXIST on the
head — a required status-check context that never produced a check run at all
(its workflow died at `startup_failure`, or the workflow simply never
triggered on this branch) contributes NO ROW, not a red one, so the command is
systematically biased toward looking green. Before repeating "landed" in this
final report, cross-check coord's PR-status surface (`coord_pr_status` /
`/pr-status` skill) — it distinguishes a genuinely satisfied required-check
state from one that was simply never established, which a clean `gh pr checks`
read cannot tell apart.

Surface the `/rename <name>` line from `/name` in the report so the operator can
update the live title bar of the current session if they want it (persisting the
title does not live-refresh the running session's title bar).

Finally, **release the plan reserve** taken at Step 1.1 — the call and the
acquirer-only rule are documented there. Do this in the same try/finally that
covers the whole chain, so an abort at Step 3 or a failed implement half releases
it too; a reserve stranded until TTL is the first operator-visible symptom of
getting this wrong ("I cannot vet my own plan"). **The release is the `finally`;
the verdict above is the `try`** — an INCOMPLETE chain releases on exactly the
same line a complete one does. There is no branch of Step 5 that reaches its end
without this call having run.

Two things wrap that release, both specified in Step 1.1:

- **Before any closeout write** — the release itself, and any gate attestation,
  work-unit transition or finding this report carries — run
  `bash <workspace-root>/qontinui-claude-config/scripts/coord-agent-refresh.sh` once and act on its verdict line. This is
  rule (c); hours have passed since rule (a) ran at the reserve, and the token
  that authenticated then may not authenticate now.
- **Inside the same try/finally, before the release call**, `remove` the
  `plan:<plan-stem>` row from the heartbeat ledger and `stop` the loop. A loop
  still running past the release re-arms a key this chain no longer owns.

If the release answers `not_held`, report it as a **lapse** per Step 1.1 — with
the since-when read out of the ledger — not as a clean idempotent no-op.

## Rules

- **Thin orchestrator only — with one deliberate exception.** Never re-implement
  vetting or implementation logic inline. Call the two skills; let each own its
  coord wiring and stamps. The exception is the Step 1.1 **plan reserve**: this
  command writes (Step 1's untracked-plan push) before either skill runs, and
  exclusion must precede mutation, so the reserve is taken here on the same key
  both skills use. Their re-reserves renew under the same owner token. **That
  stays the only exception** — Step 5's pre-PR-review check is not a second one,
  because it implements nothing: it asks whether the artifact `/implement-plan`
  owes exists for this session's id and reports INCOMPLETE when it does not. A
  gate on the hand-off is thin; a copy of the arm logic would not be.
- **Report the gate, do not enforce it by stopping.** The Step 5 refusal is a
  status, not a control-flow exit. It never suppresses the `/name` call and never
  skips the reserve release — reporting a missing artifact by stranding a reserve
  would trade a disclosed gap for an undisclosed one.
- **Reserve before the first write.** A `held` by a different owner is a STOP,
  and a reserve coord could not answer is UNKNOWN — fail closed. The only branch
  that proceeds unreserved is a device with no machine UUID at all.
- **Same path through both halves.** Resolve the plan path once in Step 1 and
  pass that identical path to both skills. The Step 1.5 session label is derived
  from that same resolved path.
- **Labeling is best-effort, never gating.** Neither the Step 1.5 provisional
  label nor the Step 5 `/name` final label may block, slow, or abort the vet →
  implement chain. If the session id or transcript file is missing, skip
  silently; if `/name` finds no PRs, keep the Step 1.5 slug.
- **Never narrate the hand-off.** Do not write "proceeding to
  `/implement-plan`", "now implementing", "moving on to implementation", or any
  equivalent as the last text of a turn. Narration is not action, and this
  substitution is the single most common way this command fails: the agent
  stamps VETTED, writes the sentence, and ends the turn without ever calling the
  Skill tool (diagnosed 2026-07-28, reproduced live in the diagnosing session).
  **The Step 3 VETTED confirmation and the Step 4 `Skill: implement-plan` call
  MUST occur in the SAME assistant turn, with the Skill call LAST.** If you find
  yourself about to write that sentence — **call the tool instead.** The tool
  call IS the sentence.
- **No finished-looking report before implementation completes.** `/vet-plan`'s
  Step 6 report is collected as an intermediate result (Step 2), never emitted
  mid-chain — a finished-looking deliverable at the midpoint is a stop cue. This
  is a rule about the MIDPOINT, not a cap on output: `/implement-plan` still
  emits its own report at the end of its run, and Step 5 adds the combined
  summary on top. What must not exist is a report between the VETTED stamp and
  the `Skill: implement-plan` call.
- **The VETTED gate is mandatory.** Never run `/implement-plan` from this
  command unless Step 3 confirms a fresh `Status: VETTED` block. A vet abort
  (closed plan, or wrong architectural direction) stops the chain.
- **One session, no stop between halves.** Like `/implement-plan` itself, the
  vet → implement chain runs end-to-end without handing back to the operator
  between the two — except for the escalations the underlying skills already
  define (operator-resource needs, oversize-plan handoff, a vet abort).

## Backstop

A `Stop` hook (`scripts/vet-imp-continuation-guard.sh`, registered in
`.claude/settings.json`) catches a chain that stalls anyway. On each stop it
checks whether this session invoked `/vet-imp` and whether the plan it operated
on is still stamped `Status: VETTED` — i.e. `/implement-plan` never reached its
Step 0.5 IN PROGRESS stamp — and if so returns `{"decision":"block"}` telling the
agent to invoke `Skill: implement-plan` now.

It fires **at most once per session** (latched by
`~/.qontinui/vet-imp-guard/<session-id>`, created before the block is emitted)
and **fails open on everything else**, so it can nag but never trap. Treat it as
the last line of defence: the rules above are what should prevent the stall, and
a hook block means they were ignored. If the block is genuinely wrong (the vet
aborted, or the operator stopped the run), state that reason in one line and
stop — it will not ask twice.
