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
2. **PRs** authored by this session (`/pr-status`, `mine=true` — fresh from
   coord's twin, never from memory). This list is not filtered by state: a PR
   that is no longer open can still appear, with `pr_state: merged` if coord
   landed it (coord stamps that for both land shapes) or `closed` if its author
   closed it (usually — but coord's land provenance can lag the close, or be lost on
   a dying land until a backfill heals it, if one does; the close comment, not
   `pr_state`, says whether a close was deliberate). Or it can be gone once its
   worktree is released. In every case nothing on the card covers commits pushed
   to its branch after the close; the WATCHED rule below the table is what
   catches that.
3. **Registered coord gates** anchored to this session's work (`/gate-sweep`).
4. **The transcript itself** — the residue. Anything that appears *only* here is
   by definition a candidate DROPPED item.
5. **The plan document itself, as its own unit.** The `.md` this session vetted,
   stamped or shipped is a unit of work, not a byproduct of one — and it is the
   unit sessions systematically forget to classify, because the code landing
   *feels* like the whole delivery. It is not: with no archive directory, git
   history on `origin/main` is the only thing preserving a finished plan
   (`/implement-plan` Step 6 item 2), so an unlanded plan file is a lost record
   even when every line of its code is on `main`.

   Classify it by the SAME `origin/main` content check the table demands below,
   after a `fetch`: the stamped file's content hash must equal `origin/main`'s
   blob at its path (`/implement-plan` Step 6 item 3's read-back, with its
   non-empty guard). An existence check is not enough, because after a stranded
   stamp push an earlier, unstamped version is already at that path. **A plan that is committed and pushed is not thereby LANDED**: a bare
   `git push` lands it on whatever branch the plans checkout was on, and nothing
   opens a PR for that branch or merges it. Measured 2026-09-02 on
   `qontinui-dev-notes`: 45 plan files reachable only from unlanded remote
   branches, the oldest ~4 months stale. A plan whose read-back fails is
   **DROPPED** — route it to a store in Step 2 by landing it per
   `/implement-plan` Step 6 item 3, which is the durable store for this class.

Now put **every** unit into exactly one terminal state:

| State | Meaning | Evidence required |
|---|---|---|
| **LANDED** | On `main`, verified **by content** on `origin/main` | The landed SHA plus the content check. `gh` PR state is not evidence in EITHER direction — `closed, merged=false` and `MERGED` with `mergeCommit.oid == headRefOid` are **both** normal coord lands (`coord-ff-lands.md`). Ancestry only on the LANDED sha, never the head |
| **WATCHED** | Incomplete, but a coord gate watches the trigger and can resume it after every session dies | `gate_id`, **read back** after registration (`coordination` `gate-read-back`) |
| **RECORDED** | Not resumable work, but a durable record exists so the next session need not re-derive it | The memory record id / plan slug / policy clause |
| **IMPEDED** | Not done because a condition that is **currently true, environmental and shared** blocks it — and that condition is now posted where the next session will be handed it | The returned `finding_id`. Cite an `alert_key` too *if you actually have one* — `GET /coord/alerts` takes a device JWT, and coord#1601 added a machine arm over a closed allowlist (`FLEET_INFRA_MACHINE_KINDS`), but that arm is **legacy-posture only** and production runs `COORD_ALERTS_TENANT_STRICT=1`, so it never fires. What decides whether you can read this class is your principal's **tenant**, not its kind — measure yours before reading a quiet result in either direction: from the system tenant it is a real read, from an ordinary tenant it is UNKNOWN. Table: `coord-gates-and-access.md` -> "A `200` from a fleet read is not a COMPLETE answer"; mechanics in the alerts note under 2b-findings. Where the blocker is an agent-responder alert, read it from the agent queue, `GET /coord/alerts/queue` (same visibility): a live claim on it means another agent is working it, so cite the row and its claimant rather than working it in parallel. A `404` there, or a body naming `schema_migration_pending`, means the queue is not served yet: cite from `/coord/alerts` and say so. Any other `5xx` is transient: retry |
| **DROPPED** | Exists only in this transcript | — |

IMPEDED is a *converted* state, not a softer DROPPED: it is earned by the
returned id, never by the blocker being real. An item whose post returned no
`finding_id` is **DROPPED**, exactly as a gate you did not read back is.

**A commit on a pushed branch that is not on `origin/main` by content cannot be
WATCHED unless a PR carries it.** Whether one does is decided by
`knowledge-base/qontinui-specific/coord-ff-lands.md` → "Pushing to a branch
whose PR may already have landed":
- An OPEN PR is necessary but not sufficient (phantom-open).
- A CLOSED or MERGED PR carries nothing pushed after its close.
- Landed-ness is per-commit `git patch-id --stable`, never
  `git merge-base --is-ancestor`, because after a rebase-land your SHA is never
  on `main`.

Read it with `gh pr list --head <branch> --state all --json number,state,headRefOid`.
Without `--state all`, an empty answer cannot tell "no PR yet" from "PR closed
under you". A carrying PR alone does not make the commit WATCHED: the table's `gate_id`
evidence still applies. A `pr_merged` gate on a PR that no longer carries the
commit does not watch it, and neither does a `commit_live` gate on that PR's
post-land SHA, because both clear on the land itself. Such a commit is
**DROPPED**; route it by that section's fresh-branch path. Plan
`2026-08-30-a-closed-pr-strands-every-later-push-to-its-branch` measured at
least 27 such branches fleet-wide on 2026-09-01.

**Run the per-branch check above for EVERY branch this session pushed to, and
classify from it.** It is the only check that sees both stranding shapes, and
nothing below replaces it or lets you skip it for any branch. If the check
itself fails on a branch — `gh` unauthenticated, rate-limited, refusing —
nothing below can substitute for it. A commit already shown to be on
`origin/main` by content is LANDED and needs no PR at all, so the failure
changes nothing for it. Every OTHER commit on that branch is **DROPPED** on
the failure alone: name the failure in the record, because an unverifiable
commit captured is recoverable and an unverifiable commit assumed landed is
not. The classification does not wait on the record landing anywhere; Step 2
owns where it goes.

**Then, additively, the server-side cross-check.** That plan's Phase 3 merged
to `origin/main` as `qontinui/qontinui-coord`**#2076** on 2026-09-11, adding
an `unproposed_branches` list to `coord_query_train_health`. LANDED is not
SERVED — whether the coord instance answering YOU carries it is a separate
fact you read rather than assume. Measured on
`qontinui/qontinui-dev-notes`: **absent** at 2026-09-11T23:50Z, **present**
(`[]`, `unproposed_branches_total: 0`,
`unproposed_branches_truncated: false`) at 2026-09-12T00:41Z — the deploy
went through in between. That is a dated observation, not a guarantee for
your box: read the table below rather than expecting any particular row from
it. If `coord_query_train_health` is not a visible tool, load it with
`ToolSearch` first. Then call it once per repo this session pushed to, with
`repo` as the full `owner/name` slug (`qontinui/qontinui-dev-notes`), not the
bare name every other line here uses. Classify the RESPONSE before you read
anything into a branch:

| What you see | What it is | Verdict |
|---|---|---|
| the `unproposed_branches` key is **absent**, and the payload is an ordinary train-health object | the coord build serving you predates `qontinui/qontinui-coord`#2076 | UNKNOWN |
| the payload is a lone `{note}` — no `repo`, no `as_of` — naming a **tenant-blind token** | a credential problem, not a repo problem: the tenant comes from the verified identity, never from arguments, so it repeats on every repo. Retry over a tenant-bound door — the native MCP tool, or `coord-revive.sh call` against a proxy nonce, both of which carry a device identity — rather than over whatever bearer produced this | UNKNOWN after one such retry; record it as a credential condition, not as a repo fact |
| the payload is `{repo, as_of, note}` naming that repo as outside your tenant's **coord authority** | TWO artifacts look exactly like this before a real one does. (a) A bare slug: the authority test is exact string equality against `canonical_repos ∪ tenant_repos` with no trimming and no owner-prefixing, so `qontinui-dev-notes` lands HERE rather than erroring — re-issue with the full `owner/name`. (b) A credential that is not tenant-bound: a door the resolver marks `PARTIAL` answers every tenant-scoped read vacuously, and **it answers exactly this note**. So the test is the door, not the repo count: only a correct slug over a NON-`PARTIAL` door makes this a statement about the repo. Seeing it on every repo you tried is corroboration of a credential artifact, not the test — one repo is the ordinary closeout | UNKNOWN. Once both are excluded, stop expecting it for that repo for the rest of this closeout — but never write it down as a permanent fact; authority is mutable state |
| the key is present and **`null`** | coord's own record read failed. If `superseded_candidate_heads_basis` beside it says the merge-proposal tables are missing, no derivation ran at all. (`table_provisioned` is NOT this discriminator — it reports the CI-samples table and appears on ordinary payloads too) | UNKNOWN |
| a client-side **`InputValidationError`**, naming no accepted arguments | the tool's schema is not loaded — nothing was sent. `ToolSearch`, then re-issue | not a reading yet; UNKNOWN if it recurs after one load |
| coord answers **`unknown_argument`** and names the accepted set | your argument name is wrong — and this also PROVES the transport is live end to end. Fix the name against what it printed and re-issue ONCE | UNKNOWN if it refuses again |
| the tool is **unreachable**, or answers `Command failed with no output` | a dead cached transport, not a verdict — but reviving it is `/coord-revive`'s job, not this step's. This cross-check is additive and every commit already has its terminal state, so do NOT spend the closeout resurrecting a door for it. Resolve the door only if this closeout needs coord for something else (a finding, a gate, a memory write); those steps carry their own recovery | UNKNOWN |
| the key is present and holds a **list** | a reading | read it as below |
| **anything else** | not a reading | UNKNOWN |

Record in the closeout which row you landed on. **No row of that table
classifies anything by itself** — a reading only corroborates, or CHANGES an
already-classified commit toward capture (the asymmetric rule below).
Nothing here produces a first classification and nothing here clears one;
the only transition it can cause is landed → DROPPED. An UNKNOWN therefore never
leaves a unit unclassified: the per-branch check has already given every
commit its terminal state.

**What a list adds, and only adds.** Each entry is coord's observation at
`last_seen_at`, not a live read, and coord proves containment with
`git merge-tree` tree equality rather than the per-commit
`git patch-id --stable` this section uses — the same PROPERTY by a different
algorithm, so the two can disagree.

- An entry naming a branch you pushed to, whose `tip_sha` is the tip you
  pushed, **corroborates** a DROPPED you already reached. Match on `tip_sha`,
  not on the branch name alone: the list is already scoped to one repo, so a
  name match is the same remote ref — but the recorded tip may be a PEER's
  push to a branch you share, not yours. A name match with a DIFFERENT
  `tip_sha` is UNKNOWN — your own later push that coord has not re-probed, or
  a peer's — and on its own decides nothing.
- **Disagreement is asymmetric, because only one direction loses work.**
  Coord naming COMMITS you classified as landed — its `commits` list, not
  the branch as a whole; this section is per-commit throughout — wins
  **whatever the entry's `tip_sha` says**. Match them the way the entry
  allows: each element is `{short_sha, subject}` and carries **no full sha**,
  `short_sha` being what `git log --format=%h` printed, so test whether your
  commit's full sha STARTS WITH that `short_sha` (and corroborate on
  `subject`). Comparing 40-character shas against this list matches nothing,
  ever, and reads as "coord corroborates nothing" — which is the one failure
  this rule exists to prevent. **A no-match is only conclusive when `elided`
  is 0.** The listed commits are the OLDEST of the run coord examined, not
  the newest: coord walks the branch oldest-first and keeps the first 20, so
  `elided` counts the ones nearest the tip — which, on a branch this session
  pushed to, are this session's own. If `elided > 0` and none of your commits
  matches, the entry still may name them and the list cannot tell you.
  Treat that as coord naming them, for the reason this bullet gives
  throughout: over-capturing costs a record, under-capturing loses the work.
  That is also the one place the previous
  bullet's "a tip mismatch decides nothing" is overridden, because
  over-capturing costs a redundant durable record and under-capturing loses
  the work. **It moves CAPTURE, not remediation:** treat those commits as
  DROPPED for Steps 2 and 3, and let the fresh-branch path's own per-commit
  guard ("if nothing is unlanded, nothing is owed") decide whether a new PR
  is actually owed — coord's entry is an observation at `last_seen_at` and
  the re-probe is memo-bounded to about one per branch per hour, so a strand
  that landed since can still be listed, and opening a PR off this alone
  would re-propose landed commits. **One case that guard cannot settle,
  because the guard IS per-commit patch-id:** a commit whose patch landed and
  was then reverted. Patch-id matches it on `main` and the guard says nothing
  is owed, while coord's `merge-tree` against the live base correctly says
  the content is not there. Do not let the guard's silence end it — keep the
  capture, and post a coord finding naming the two verdicts and the branch.
  That is the durable store Step 2 would give it anyway, and it needs no
  operator to read this transcript.
  The other direction is the harmless one: coord silent on a branch you
  classified DROPPED changes nothing, because coord cannot clear a strand
  (below). Record the disagreement either way, **with the entry's
  `last_seen_at`**: an entry older than your last push MIGHT be explained by
  staleness and an entry newer than it cannot, so the timestamp is what says
  whether staleness is even available as an explanation. It never decides the
  case on its own — the algorithm difference above produces the same
  disagreement at any age. This list exists partly to be measured.
- The strand's size is `len(commits) + elided`, never `len(commits)` alone:
  the listed commits are capped and `elided` carries the rest. **Report it as
  "at least N" unless you actually read `boundary_beyond_walk: false`** —
  that observed `false` is the only thing that makes the sum exact. `true`
  means the boundary walk hit its cap, so the sum is a floor. Absent or
  `null` means you cannot tell, and you report that the same way a floor is
  reported; an absent key is NOT `false`. Absent is the case you will usually
  meet for as long as the serving build predates
  `qontinui/qontinui-coord`#2077, which added the projection (merged
  2026-09-12; #2076 had written the key into coord's record and left it off
  this door). Treat that as background rather than as the test: the test is
  what the payload in front of you carries. If `elided` is `null` but `commits` is a list,
  `len(commits)` is still a floor — use it. If `commits` itself is `null`
  there is no N at all: say the size is UNKNOWN and name the branch anyway.
- **`cause` narrows what the strand is owed — and narrows LESS than its names
  suggest.** The values are coord's `UnproposedCause` enum, and it is computed
  from exactly two inputs: whether a PR ever named the branch, and whether any
  of the branch's content is already on the base. Do not expect the
  `grew_after_land` shape the exclusions bullet further down uses as its
  example: all three entries `qontinui/qontinui-claude-config` served at
  2026-09-12T03:24Z read `grew_after_unlanded_close`.
  - `never_proposed` — no PR ever named this branch, and its newest push is
    over 24h old. Owed a PR, subject to the per-commit "if nothing is unlanded,
    nothing is owed" guard. This is the only value that forecloses the
    deliberate-close exception below, and it forecloses it because there is no
    PR to have been closed.
  - `grew_after_land` — a PR named the branch, reached a terminal state, was
    pushed to afterwards, and **some** of the branch's content is on the base.
  - `grew_after_unlanded_close` — the same, with **none** of it on the base.
  **The cause does not read the close reason, so it cannot tell you whether the
  close was deliberate.** `grew_after_land` says only that content containment
  found something; the candidate is any PR in a terminal state, landed or not.
  So on **any** entry carrying a `pr_number`, read the close comment before
  routing it, per `coord-ff-lands.md`'s **"Exception: a deliberate close"** — a
  superseded or abandoned PR is recorded, not re-proposed. The commits are
  still **DROPPED** and still owed a durable record in Step 2 either way; what
  the close comment decides is whether a new PR is owed at all. Say which you
  concluded and on what evidence — "coord listed it" is not that evidence, and
  neither is the cause name.
  `landed_through` is **not** that PR: it is the abbreviated sha of the newest
  commit on the branch already on the base — the boundary the strand starts
  after. The PR is `pr_number`, always. A `null` `landed_through` therefore
  never means an absent PR, and it does not mean a floor either: it is written
  for **two different reasons** and only one of them is a floor. Either no
  boundary was found because none of the branch is on the base (the size is
  EXACT), or the backward walk hit its cap before reaching one (a floor). The
  field that separates them is `boundary_beyond_walk`, exactly as the bullet
  above says — read that, not this one, to decide "at least N". Do not infer
  the cause from it in either direction: a capped walk makes
  `boundary_beyond_walk` true, which reads as "something landed" and yields
  `grew_after_land` — but only where a `pr_number` exists, since an entry with
  no `pr_number` is `never_proposed` whatever the walk did. A non-null
  `landed_through` does not imply a PR either, for the same reason: a branch no
  PR ever named is `never_proposed` however much of its content reached the
  base.
  A cause outside those three is UNKNOWN: name it and route the branch by the
  per-branch check alone. Nothing here overrides that check — `cause` changes
  the REMEDIATION Step 2 and Step 3 owe, never the classification Step 1
  already made.
- `unproposed_branches_truncated: true` means the list was cut at 50;
  `unproposed_branches_total` minus the list's length is how many you did not
  see. Those are UNKNOWN, not absent — and the omission is **not random**:
  the read is oldest-`first_seen_at` first, so truncation drops the newest
  RECORDS. That is against you for a branch this session pushed after its PR
  closed, whose record would have been minted just now. It is not the reason
  a never-proposed branch of yours is missing (that is the 24h hold below),
  and a branch coord recorded earlier has an old `first_seen_at` and survives
  the cut.

**A branch missing from the list is never evidence that it is fine** — not
when the list is empty, and not when it is present, non-empty and untruncated.
Things coord's record cannot or does not name:

- a branch stranded behind a **phantom-open** PR (OPEN, but its head already
  on `origin/main` by content — the case
  `knowledge-base/qontinui-specific/coord-ff-lands.md` names). The two
  minting arms exclude such a branch in SQL and the re-probe arm's rows are
  cleared by the resolve pass that runs first on every sweep, so no route
  reaches it — and none of the three `cause` values covers it. Invisible
  **by construction**;
- a **never-proposed branch whose newest push is under 24h old**, which coord
  holds back deliberately because a fresh push with no PR is the normal
  authoring flow. This session's pushes are minutes old, so a branch you
  pushed, never proposed, and that coord holds **no existing record for**
  cannot appear yet. A branch coord already recorded is re-probed with no age
  gate, so it can appear at your newest tip — that entry is a real match, not
  a collision;
- branches excluded from the **`never_proposed` arm only** — the fixed
  default-branch NAMES (`main`, `master`, `develop`, `trunk`), `gh-pages`,
  `production`, `release/*`, `backport/*` and coord's scratch refs. The
  post-close arm (the two `grew_after_*` causes) applies none of these, so a
  `release/*` branch whose PR landed and then grew IS recorded, as
  `grew_after_land`;
- anything the sweep did not serve on its last tick — rows past its 500-row
  fetch page, or branches deferred by its per-arm probe budget or its probe
  deadline. Coord publishes that remainder as the
  `coord_landed_branch_grew_backlog` gauge.

So the cross-check can confirm a strand and can never clear one. The
per-branch check at the top of this block is what clears it.

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
  For a CAPABILITY negative ("no door", "agents cannot", "no such route") the
  re-verification IS the census below — a probe of one URL does not count:

<!-- detector-reach-fence:start -->
> **A capability negative cites a CENSUS, never a probe.** Before recording
> "no door", "agents cannot", "this route does not exist" or any other claim
> that a capability is ABSENT, run `bash scripts/coord-route-census.sh
> <fragment>` (qontinui-claude-config; reads `origin/main` of BOTH
> `qontinui-coord` and `qontinui-web`, never a working tree and never a live
> host) and paste its trailer verbatim beside the claim:
> `census: fragment=<f> hosts_read=coord.qontinui.io,api.qontinui.io ref=<sha>,<sha> routes=<n> unextracted=<n> unmounted=<n> generated=<ISO time>`
> — the line that parses under `CENSUS_TRAILER_RE` in
> `scripts/detector_reach/__init__.py`. A 401, 404 or 405 on ONE spelling of
> ONE host is a sample, not a search: `/api/v1/memory` refuses on
> `coord.qontinui.io` and answers on `api.qontinui.io`. A claim without the
> trailer is **UNVERIFIED and is not recorded** — not as a finding, not as a
> memory, not as a plan premise. `routes=UNKNOWN` (exit 2) means the census
> could not read a source and settles nothing; `routes=0` with both refs
> resolved is the only honest negative, and `admits=unknown` on a listed row
> means unmeasured, never "operator-only".
<!-- detector-reach-fence:end -->

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
**`coord_recent_findings`** for the `resource_keys` you are about to touch, *or*
for the `topic` when you know the subsystem before you know the files. Findings
are pull-by-relevance: nothing pushes them at you, so a session that never asks
is told nothing.

> ⚠️ **`resource_keys` and `topic` are OR'd, not AND'd — passing both WIDENS the
> read.** coord's `recent` matches
> `f.resource_keys && $2 OR ($3 IS NOT NULL AND f.topic = $3)`
> (qontinui-coord `crates/coord/src/findings.rs`), so a call carrying both is a
> **union** and returns **more** rows than either alone. There is no way to ask coord for
> *"findings about these files, **within** this subsystem"* — if you need the
> intersection, read twice and intersect yourself.
>
> This matters here more than anywhere else the pair appears. The whole point of
> this section is that a wrong-**width** answer arrives looking like data rather
> than like an error, and a reader who adds the `topic` believing it *sharpens* the
> probe gets the opposite and cannot tell, and **nothing upstream says otherwise**:
> `coord_recent_findings`' own tool description reads *"Filter by resource_keys …
> **and/or** topic"*, which carries the identical ambiguity. `coord-read.ps1`'s
> `findings` verb prints a union warning on stderr when it is handed both; over MCP
> the reader is the only guard.

**This probe has the same two doors the post does — use them.** If
`coord_recent_findings` is masked or its transport is dead, read the twin:
`GET /coord/agent-findings?resource_keys=…&topic=…&limit=…` (repeatable *and*
comma-separated keys; `limit` defaults to 20 and is clamped to `1..=100` rather
than erroring — and the two filters are the same **OR** as above, so that URL
spelled with both is a union, not a narrowing). Both call the same
`findings::recent`, so this is the same answer, not a lesser one — the full
cascade, and the credential to drive it, are under **2b → Transport** below.

> ⚠️ **Comma-separated keys are an HTTP-ONLY spelling — do not carry them to MCP.**
> The two doors run the same *query* and deliberately do **not** run the same *input
> parsing*. A query string has no array type, so `resource_keys=a,b` must mean two
> keys and the HTTP door splits it; a JSON body and an MCP argument already carry an
> array, and splitting there — in coord's own words — *"would turn one key containing
> a comma into two"*, so the MCP door does not split
> (`crates/coord/src/findings.rs`, `normalize_resource_keys` vs
> `normalize_query_resource_keys`).
>
> Hand `coord_recent_findings` the string `"a,b"` and it filters on **one literal key**
> `a,b`. `recent`'s `&&` is EXACT element equality, so that matches nothing and answers
> `count: 0` with `available: true` and `resource_keys_truncated: false` — a clean,
> confident, **wrong zero**, produced by reading the HTTP recipe one line too far. Over
> MCP, pass an **array**. This is this section's own failure mode landing on this
> section's own recipe.

A probe that is mandatory before asserting an environmental claim must not be
the one step with a single point of failure, and
the session that cannot reach MCP is precisely the one holding a condition worth
looking up.

**An empty result is two different answers, and the response tells you which —
on either door.** The response is six fields — `{count, findings, limit,
resource_keys_applied, resource_keys_truncated, available}` — and **three of them
are honesty bits you have to read yourself**:

| Read | When it says | Because |
|---|---|---|
| **`available`** | `false` → the answer is **UNKNOWN**, not "nothing was filed" | `coord.findings` is not provisioned [policy: `verification-and-evidence` `silent-empty-is-unknown`] |
| **`count` vs `limit`** | equal → the page was **FULL**, so there may be more | `limit` is echoed back beside the row count, which makes this a complete truncation test at *any* page size. **`limit` defaults to 20**, so a probe that comes back with 20 rows has told you nothing about row 21 — raise it (clamped `1..=100`) or narrow the filter |
| **`resource_keys_truncated`** | `true` → the pull is **partial** | a distinct key was actually **dropped**. The ceiling is 100 (`RECENT_MAX_RESOURCE_KEYS`) and the guard fires on the 101st, so a pull naming *exactly* 100 is complete and correctly reports `false` |

Those six fields have their **own** provenance, later than the door's: the G0 route
landed at `61a107be` (the sha 2b → Transport still cites) serving four, and `count`,
`limit` and the *"READ `available` BEFORE `count`"* guidance arrived at `70aedbb7`
on 2026-08-24. A build older than that answers a smaller shape — measured 2026-09-01,
production serves the six.

Read all three before you read `findings`. This is the read-side mirror of the
`{"posted": false}` trap under 2b, and it fails in the more dangerous direction:
a write that silently stored nothing is at least still in your transcript, while
a read that silently returned nothing becomes the evidence you re-derive a whole
condition from. The `count == limit` row is the one that used to be missing here,
and it is the one a *successful, provisioned, untruncated* read can still fail on.
It is missing upstream too: the tool description says **"READ `available` BEFORE
`count`"** and then never says what `count` should be compared against.

This clause exists because a real session re-derived a standing
`/api/v1/plan-library` 404 from scratch **while a memory line naming that exact
404 was already in its context**. Knowing a thing and being told to look are not
the same, and this command previously told you to look nowhere.

If a returned finding already covers the condition, **cite it and move on** — do
not repeat the investigation. If your own evidence *corrects* it, post the
replacement with `supersedes` set to the stale `finding_id`, so reads return one
live head rather than two contradictory rows.

**Probe again immediately BEFORE each post — the first probe is not enough.**
A closeout runs for tens of minutes between reading and writing, and peers close
out concurrently. Measured 2026-08-25: a session probed `topic=plan-corpus`, got
one finding, worked for ~40 minutes, and posted a correction — not knowing a peer
had filed the same correction 40 minutes into that window. Two live heads saying
the same thing, which is precisely what `supersedes` exists to prevent.

The deconfliction surfaces cannot catch this: `coord_who_is_working_on` answers
"who has DECLARED this", not "who has just DONE it", and a peer who finishes
inside your window appears in neither. So the re-probe is the only guard, it
costs one call, and it belongs against the `resource_keys` you are about to
write — not against the ones you started from.

When the re-probe shows a peer got there first, do **not** publish in full and do
**not** suppress: publish the **delta**, per [policy: coordination
`concurrent-duplicate-discovered-at-write-time`]. Keep the peer's row as the
primary record, carry only what your run produced that theirs could not — a
different method, a different timestamp, a contradiction — and say which row is
primary. Superseding a peer's correct work is a different decision, permitted
only when that row is factually wrong; superseding **your own** near-duplicate to
collapse it to the delta is always allowed and is the repair when you notice too
late.

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
  from a usable one: a returned `gate_id` is **not** sufficient. **Branch on the
  VERDICT, never on `warnings[].is_empty()`.** Treat the gate as **not
  registered** for the purposes of Step 2a — the item is still DROPPED and must
  fall through to 2b/2c/2d — when `initial_verdict_reason` says the predicate
  **cannot be evaluated**, or when `initial_verdict` is a terminal state it can
  never clear from (`misconfigured` / `failed`).

  **A non-empty `warnings[]` is NOT that signal — read it, do not count it.**
  Most warnings are informational, and dropping on their presence throws a
  WATCHED item back into DROPPED, which at a session-close door is the expensive
  direction. Two you will meet routinely: a `pr_merged` gate on a
  coord-orchestrated repo *always* comes back carrying coord's informational
  `ℹ … the clear may lag GitHub's close by one provenance write` steer, which
  `check_predicate` pushes into **`warnings[]`** and not only into `steer`
  (`gates.rs`, `pr_merged_orchestrated_warning` → `warnings.push`); and
  `continuation_dropped_born_cleared:` reports that only the CONTINUATION was
  refused, on a gate that clears fine. Both are steers, **not rejections**: the
  gate is registered and usable. Read the warning text before dropping anything.
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
  topic         = "plan-corpus",
  resource_keys = [ "<the files/globs/PR/plan-slug a peer would be working>" ],
  dossier_slug  = "<slug>",
  title = "<the one-sentence CLAIM a reader sees when bodies are projected away>",
  body  = "<evidence, then method, then what a peer should do differently — in that order>")
```

Those are the argument names on both doors — the HTTP twin under **Transport**
below takes the same fields as a JSON body (`title` and `body` required), so
this shape is what you send either way.

**`topic` is a TAG, not a headline, and the shape is a rule.**
`^[a-z0-9][a-z0-9-]*(:[a-z0-9-]+)?$` — lowercase kebab, 2-64 bytes, one optional
`:`-namespaced suffix. `merge-engine`, `coord-deploy`, `plan-corpus` and
`dossier:<slug>` all parse; `coord_work_unit_list filter and truncation honesty`
never does. This is not style: **every read door matches `topic` by EXACT string
equality, never by prefix or substring**, so two spellings of one subsystem are
two disjoint tag spaces and neither session ever sees the other's row. Pick the
tag a PEER would guess, not the one that describes your incident.

**`dossier_slug` when this is the Nth occurrence** of a known pattern — whether
or not a `dossier:<slug>` head exists yet. It means *"this occurrence belongs to
that pattern"*, and it is what lets `/findings-steward` fold the row without
re-deriving the cluster. It is a **top-level argument** on both doors, and its
shape is the `topic` grammar **minus the colon** — `^[a-z0-9][a-z0-9-]*$`,
lowercase kebab, 2-64 bytes, **no `:` anywhere**, which is a DIFFERENT rule from
`topic`'s, not the same one. That rules out the `dossier:` prefix — that prefix
belongs on the head's `topic`, and coord refuses it here, and offers the
corrected value when the remainder is itself a legal slug
(`dossier:merge-engine` → *send `merge-engine`*). When the remainder is NOT a
legal slug the refusal is the bare shape message with no correction at all —
`dossier:`, `dossier:a`, `dossier:merge:engine`, `dossier:dossier:foo`,
`dossier:-lead` — because a suggested correction that is itself refused is
worse than none. But it also
rules out `merge:engine`, and the reason is the part that makes it stick: the
head's own `topic = "dossier:<slug>"` already spends the single `:`-namespaced
suffix a topic may carry, so a slug containing a colon would key a head topic of
`dossier:merge:engine` — three `:`-separated parts, which the write door
REFUSES. Such an occurrence could never be folded into any head, so coord
refuses the colon here rather than storing dead data. Use `-` where you reached
for `:`. A present-but-**blank** `dossier_slug` is refused too, not silently
dropped — omit the argument entirely when you have no slug; absent stays
absent-and-fine.
Coord STORES it at `artifact_refs.dossier_slug`, which is where every read
projection and `/findings-steward`'s classifier looks for it, so writing it
inline in `artifact_refs` yourself is the equivalent coord also accepts and
validates identically (the explicit argument wins when both are given). Prefer
the top-level argument: it is the spelling the tool schema documents and the
one a refusal names.

The write door's schema is `additionalProperties: false`, so an unknown
argument name is REFUSED **with the accepted set returned** — read that refusal
rather than a list written down here, which goes stale the next time coord
deploys. That is also the one case where the `artifact_refs` spelling is not
merely equivalent but required: a coord deployed BEFORE the typed field lands
answers exactly that refusal, with `dossier_slug` absent from the set it names.
Re-send it inside `artifact_refs`; the row is identical either way.

**Body order is evidence, then method, then what a peer should do differently.**
The title is what survives projection — a steward or a boot-time pull reads
titles and never your body, so a title that says "investigated X" tells the next
session nothing while "X is Y because Z" tells it everything.

**One finding per condition per session — correct with `supersedes`, never post a
second.** A second row for the same condition splits the evidence between two
ids that no read can join, and `supersedes` is resolved server-side against your
own tenant, so a correction chain is cheap and a duplicate is not.

**Precondition for a DOOR-SHAPED `status` / `gotcha`** (a finding whose body
says a route, tool or host is absent, refuses, or cannot be reached by agents):
the census trailer is part of the body, or the finding is not posted.

<!-- detector-reach-fence:start -->
> **A capability negative cites a CENSUS, never a probe.** Before recording
> "no door", "agents cannot", "this route does not exist" or any other claim
> that a capability is ABSENT, run `bash scripts/coord-route-census.sh
> <fragment>` (qontinui-claude-config; reads `origin/main` of BOTH
> `qontinui-coord` and `qontinui-web`, never a working tree and never a live
> host) and paste its trailer verbatim beside the claim:
> `census: fragment=<f> hosts_read=coord.qontinui.io,api.qontinui.io ref=<sha>,<sha> routes=<n> unextracted=<n> unmounted=<n> generated=<ISO time>`
> — the line that parses under `CENSUS_TRAILER_RE` in
> `scripts/detector_reach/__init__.py`. A 401, 404 or 405 on ONE spelling of
> ONE host is a sample, not a search: `/api/v1/memory` refuses on
> `coord.qontinui.io` and answers on `api.qontinui.io`. A claim without the
> trailer is **UNVERIFIED and is not recorded** — not as a finding, not as a
> memory, not as a plan premise. `routes=UNKNOWN` (exit 2) means the census
> could not read a source and settles nothing; `routes=0` with both refs
> resolved is the only honest negative, and `admits=unknown` on a listed row
> means unmeasured, never "operator-only".
<!-- detector-reach-fence:end -->

`kind="status"` for *this is how the world is right now*; `kind="gotcha"` for an
operational trap a peer will walk into. `scope="fleet-infra"` **only** when the
condition is a truth about **shared** coord/runner behaviour — anything
tenant-local stays `scope="tenant"` (the default). `resource_keys` is not
decoration: it is the overlap key a peer's `coord_recent_findings` matches on.
Because the two filters are **OR'd** (see "Probe before you re-derive"), a finding
filed with none is not strictly unreachable — a peer probing your `topic` still
gets it. But topic is a subsystem guess the peer has to make *before* they know
the files, and the mandatory probe above is keyed on the files they are about to
touch, so **a finding with no `resource_keys` is one the reader most likely to
need it will not be handed.** Fill both.

**The split against 2c, stated so nothing routes twice.** Findings **expire in
~14 days**. That makes them right for what is *true now and expected to become
false*, and wrong for what will still be true next quarter. A permanent hazard —
an interface that will always mangle Windows paths, a probe that is vacuous by
construction — belongs in **memory**, distilled. Routing everything to findings
would quietly delete the durable half a fortnight later; routing everything to
memory buries today's transient condition among permanent ones. One item may
legitimately produce **both**: the *condition* as a finding, the *lesson* as a
memory. What it must never produce is a finding standing in for a memory.

**Transport — findings have TWO doors, and MCP is the one that dies first.**
Measured: a session held a working coord **device JWT** for its whole run —
work-units, gates, prompt-documents and the `/pr-merge` feed all answered — while
`/coord-mcp` returned `401` and the workspace `.mcp.json` had been deleted
outright by an ephemeral runner. That is precisely the degraded session most
likely to be holding a fleet-infra condition worth recording, and it is the
session the second door was built for. So try, in order:

1. **`coord_post_finding`** when the tool is visible **and a call answers**.
   Those are two different things through the runner's `/coord-mcp` proxy:
   `tools/list` is unfiltered while `tools/call` is gated by
   `COORD_MCP_ALLOWED_TOOLS`, so a `-32601` means *not callable here*, not
   *not shipped*. (Both findings tools are on that allowlist today —
   verified — so this arm works; the point is not to read visibility as
   proof.) **Do not wait for the refusal to point you at arm 2 — through the
   runner's proxy it structurally cannot.** Both verbs are registered in coord's
   `MCP_HTTP_DOORS`, so a refusal *coord* generates names the HTTP route serving
   the same core (before those rows existed it said "no HTTP route is
   registered" — the dead end G0 exists to prevent). But the `-32601` above is
   emitted by the **runner proxy**, locally, from the allowlist compiled into the
   running binary; the runner's own source calls it a bare `-32601` that "reads
   like *no such tool* rather than *your proxy withholds it*." **But it is not
   silent — read `error.data.code`:**
   `"COORD_MCP_PROXY_METHOD_NOT_ALLOWED"` is the proxy telling you it withheld
   the call, which is the discriminator the `-32601` alone hides. Either way the
   refusal says nothing about whether the door exists upstream — deliberately —
   so go to arm 2 on your own initiative. If the call returns `"Command failed with
   no output"`, that is a dead cached transport and the write is **presumed
   LOST** — `/coord-revive`, re-issue, verify by read.
2. **The device-authed HTTP twin — LANDED 2026-08-23, not pending.**
   `POST /coord/agent-findings`, read side
   `GET /coord/agent-findings?resource_keys=…&topic=…&limit=…` (those two filters
   are OR'd — see "Probe before you re-derive"), both verbs on one
   `.route()` on the `require_jwt` sub-router beside `agent-work-units` and
   `agent-gates` (`61a107be`, coord#1601, Phase 2/G0 — `routes.rs`
   `agent_findings_authed`, handlers `post_finding_agent` /
   `recent_findings_agent`). **So a `404` here is no longer "not shipped yet"** —
   it is a wrong path, a wrong base URL, or a serving build genuinely behind
   `origin/main`, and the last of those is the only one that is arm 3.
   - **Same core, so the shape above is unchanged.** Both doors call
     `findings::post`, which owns the size bounds and the trim/de-duplicate
     normalization, so the finding's **content** stores identically whichever
     transport you held. `title` and `body` are **required**
     (`PostFindingBody`); everything else is optional.
   - **One field does NOT match across the doors: `author_session`.** The HTTP
     door deliberately does not reproduce the MCP handler's
     `resolve_self_session` fuzzy fallback — coord rejects that bridge for
     attribution because it names the wrong parent under concurrent sessions,
     which is this fleet's normal state. Over HTTP the session is taken from the
     proxy-injected `X-Coord-Caller-Session` header and validated **fail-closed**
     as bound to your device; absent or unprovable, it stores `NULL`, meaning
     *coord could not prove which session this was* — Unavailable, never Absent.
     **And the proxy is what injects that header, so on this door — whose whole
     audience is sessions whose proxy is dead — `NULL` is the normal case, not
     an edge one.** If you know your own coord session id, forward it as
     `X-Coord-Caller-Session` and the attribution survives. Either way it costs
     you nothing for IMPEDED (the `finding_id` is what you cite), but do not
     later read a `NULL` author as evidence that no session filed it, and expect
     anything that ranks or filters by `author_session` to see arm 2's output as
     unattributed.
   - **Identity is refused, not ignored.** `tenant_id`, `author_session` and
     `author_device` are lifted from the credential, and a body carrying any of
     them is rejected with a `400` naming the field — so do not "fix" that error
     by filling more of them in.
   - **Credential — it must carry a `device_id` claim.** A verified token
     without one is refused `400` (*not* `403`) saying the finding "would have no
     attributable author", so read that as *wrong credential*, not as *denied*.
   - ⛔ **NEVER mint that credential from `POST /agents/allocate`.** This step
     used to recommend it by name, and that recommendation is **withdrawn**.
     `.claude/commands/gate.md:816` and `policy.md:323` already carry the
     prohibition verbatim — *"NEVER carry this rung on a JWT minted from `POST
     /agents/allocate`. That route is genuinely unauthenticated and mints a
     4-hour full-scope agent JWT to anyone who knows a registered device UUID"* —
     and it is an open security question that plan
     `2026-08-31-coord-mcp-credential-selection-by-binding-provenance` surfaces
     and explicitly refuses to build on (coord finding `65d574cc`).

     **This file was the gap, and it was the worst possible one:** the
     prohibition reached `gate.md` and `policy.md` but not `/unattended`, whose
     entire audience is degraded sessions with dead credential doors — i.e.
     exactly the population the ruling is for (findings `e91f21b9`,
     `31d62d46`). Measured against `origin/main` 2026-09-02: `gate.md` and
     `policy.md` carried the prohibition once each; this file carried it **zero**
     times and instructed the route **twice**.

     Two further measured costs, so the route is not merely disallowed but
     actively bad: it **leaks an unreleasable ledger row per call** — coord
     OVERRIDES `isolation.mode:none` and registers a worktree the minting agent
     cannot release, because release is operator-gated (finding `3c922516`) —
     and calling it with `repos: []` returns a **credential-shaped blank**
     (`token: ""`, reproduced 3x on 2026-09-02), while a non-empty `repos`
     returns a real token *and* the leaked row. So the shape that avoids the
     leak yields no credential, and the shape that yields a credential leaks.

     **If no legitimate door answers, the honest outcome is arm 3: the finding
     was NOT recorded.** Name the transport failure and leave the item DROPPED.
     A closeout that buys its own bookkeeping with a standing security exposure
     has made the fleet worse, not better.
   - **This token is scoped to coord's agent doors — it is not a fleet-wide
     credential.** It opens `/coord/agent-findings`; it does **not** open
     qontinui-web's plan-library write routes (3b), which refuse it `401`
     because the allocate mint carries no `user_id` claim — see the blockquote
     under 2b-bis. Do not read "my closeout finding posted" as evidence that
     your other closeout writes will.
3. **Neither answered → say the finding was NOT recorded.** Name the transport
   failure you actually saw, and leave the item **DROPPED**.

> **Corrected 2026-08-30.** Until then this section opened *"findings are
> MCP-only today"* and arm 2 said the HTTP twin *"is being added by Phase 2"*.
> Both were the driving plan's **G0 gap statement** copied here as standing
> fact, and both outlived the gap by a week: G0 shipped on 2026-08-23 in the
> very commit arm 2 now cites. The cost was not cosmetic — it told the one
> session that had lost MCP that its only fallback might not exist yet, which is
> the exact failure G0 was built to prevent. #377 corrected the *alerts* half of
> this same paragraph and left the *findings* half standing, two paragraphs
> apart, because a correction lands where it was discovered rather than
> everywhere the fact is consumed.

**Never report a finding as stored on the strength of having issued the call —
on either door.** Both answer the same envelope (duplicated deliberately, not a
shared helper): `{"posted": false, "reason": "coord.findings is not provisioned
yet …"}` when the table is unprovisioned — a **success status that stored
nothing**. Over arm 2 that distinction is also in the status line, which is the
cheapest check you have: **`201 Created` stored a row, `200 OK` did not.** Per
this command's own rule, an item with no returned `finding_id` is DROPPED, not
IMPEDED.

Read every coord/web/runner response through `scripts/lib/envelope.py` / `envelope.sh`; assert `count`-vs-rows agreement before acting on any zero; an `UNKNOWN:` line is UNKNOWN, not a negative. The
key names — `posted` first, then the id nested under `finding` — live in the
helper's docstring (`scripts/lib/envelope.py`), one home, and are not restated
here: a session that reads the wrong key off a correct envelope declares
DROPPED on a write that actually succeeded, the mirror image of the failure this
paragraph is about, and the helper cannot return that empty value.

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
device reaches that class through a pre-existing arm — `build_get_alerts_query`'s
`is_system_tenant`, which keys on the **tenant alone**, with no reference to
principal kind or `is_admin` — so a privileged principal is not a sample. So the
finding body is the edge record you actually author; the `alerts.detail` mirror
is a note for whoever owns the alert, not a step in this procedure. Never report
an edge as mirrored when no door existed to mirror it through.

**The agent door onto open alerts is now the queue, and it adds ownership, not
reach.** `GET /coord/alerts/queue` (MCP `coord_alert_queue`) serves the open
agent-responder rows with `responder_domain` and claim state; `coord_alert_claim`
/ `coord_alert_release` take and give back a lease (plan
`2026-09-18-notifications-are-agent-actions-and-alerts-are-agent-work` Phase 2;
the protocol is `knowledge-base/qontinui-specific/coord-gates-and-access.md` ->
"The agent alert work queue — claim before you act"). It runs under the **same**
visibility predicate as `/coord/alerts`, so everything above about which
principal reads which class still holds, and an empty queue is UNKNOWN on the
same terms. What it changes for this audit: claim a row before you work the
condition it names and work it only when the answer's `status` is `claimed`
(KB -> "The claim answer — act only on `status: claimed`"), release it when
done, and never report a condition *resolved* because you claimed or fixed it —
coord resolves only by re-observation. Fall back to the `/coord/alerts` read
above, and say so, only on the three answers the KB names: coord refuses the
tool as unknown, the route answers `404`, or the body or tool error names
`schema_migration_pending`. Any other `5xx` is transient — retry.

**Claim mechanics** (KB -> "The claim answer — act only on `status: claimed`"):
add the alert id to this session's claimed list when the claim is SENT, not when
it answers, so a claim retried after a `5xx` that comes back `claimed, renewed:
true` reads as yours; a `claimed_by` of `device:<d>:session:<your own session>`
is always yours. Release over the door you claimed through; a `claimed_by_other`
whose `claimed_by` equals the label you recorded is your own lease under another
label — release again over that door, or let it lapse if that door is
unavailable, never a third door. `tool_not_available_to_principal` (with an
`alternate_door`) means use the HTTP twin it names; it is not a fallback to
`/coord/alerts`.

**Measure your own principal rather than carrying either verdict.** The operator
box's device is on the system tenant and read the class from an agent JWT on
2026-08-29 (measured by ccfg#437, not re-run here); a customer tenant's paired
runner is not on that tenant and would not. The full principal x endpoint
matrix — including the asymmetry that runs the OTHER way on
`/coord/fleet/health`, where `is_admin` is tested first and unconditionally, so
an ordinary tenant's admin gets the WIDE rollup and the NARROW list — is in
`qontinui-claude-config/knowledge-base/qontinui-specific/coord-gates-and-access.md`
-> "A `200` from a fleet read is not a COMPLETE answer". Read it there; it is one
home for a fact this command, `/manual-test-coord` and `/dev-ops-steward` all
consume, and re-deriving it is how its direction got written backwards once
already (ccfg#437).

**This is not a gate, and must not become one.** A finding *records* a condition;
a gate *watches an observable trigger* and resumes the work when it flips (2a,
`/blocked`, `_gate-registration` — gates remain for observable triggers only). If
the condition has a predicate coord can evaluate, it belongs in 2a, and you may
post the finding **as well** so peers know it is true today. If it does not, the
finding is the honest store, and force-fitting a predicate onto it pollutes the
registry with gates that never clear.

### 2b-bis — Is this an instance of a RECURRING issue? → a dossier

A finding records **this occurrence**. A dossier records **the pattern**. They
are different questions, and the fleet has been answering only the first.

**Why this step exists.** On 2026-08-25 a closeout searched the stores for one
issue class — *a stale shared checkout read as a defect in shipped code* — and
found **8 memory records** (2026-05-25 → 2026-08-03), **3 findings**, and **7
plans of which 5 had SHIPPED**. It had recurred **twice that same day**, to two
independent sessions ~7h apart, each filing a fresh false "shipped code is
broken" report and then retracting it. One of those memories documents its own
recurrence in its body: *"The trap is easy to fall into even knowing this memory
exists."*

Eighteen artifacts, five shipped fixes, still recurring — and **no surface in
the fleet could show that sentence**, which is why nobody had acted on it.
Assembling it took a memory search, a findings pull and a 1,197-file `git grep`.
No session does that spontaneously. This step is what makes it happen.

Only in aggregate does the actionable defect appear. In that instance it was:
*every shipped fix targeted detection or the pull decision; none changed the read
path an agent actually uses* — a conclusion about the SET, invisible from inside
any one record. **For a recurring defect the actionable signal is recurrence
DESPITE remediation, and recurrence is a property of a collection.**

**The rule.** Before filing a finding for an environmental condition, check
whether it is the Nth occurrence — and find the dossier's **head** by its key,
not by its phrasing:

```
coord_recent_findings(topic="dossier:<slug>", kind="dossier")   # the live head — PRIMARY
#   HTTP twin: GET /coord/agent-findings?topic=dossier:<slug>&kind=dossier
coord_memory_search(query_text="DOSSIER <slug>", kinds=["mental_model"])   # the literal KEY — fallback
coord_memory_search(query_text="<the condition in its own words>")
coord_memory_search(query_text="<the condition, phrased a second way>")
```

**The finding is the primary store; memory is the fallback.** When the first
probe returns a head, **that head is authoritative** — still run the memory
probes, but only to pick up `DOSSIER-CONTRIB` / `DOSSIER-DELTA` rows not yet
merged into it, never to second-guess the finding. The memory probes are the
lookup of record in exactly ONE case: the slug's head still lives in
memory — not yet migrated; plan
`2026-08-25-dossiers-durable-issue-files-for-recurring-defects` Phase 4
migrates one per closeout, so expect this for a while.

**The "serving coord rejects the `dossier` kind" hedge is DELETED, because the
capability SHIPPED.** Verified on qontinui-coord `origin/main` `967f8474`
(2026-09-07): `dossier` is in `FINDING_KINDS`, `DOSSIER_TTL` is `100 years`
selected by `ttl_for_kind`, and `recent()` takes a `kind` filter. Measured live
the same day: `kind="dossier"` with no other filter returns **25 heads, exactly
one per slug**. So the findings probe above is the primary lookup on every
current box, and a session that skips it because a document warned the door
might refuse the kind is skipping a working door — which is what a live hedge
against a shipped capability costs. If a future box genuinely refuses, that is a
version fact to report and measure, not a branch to pre-write here.

**The head has a deterministic key.** A dossier head's title MUST be exactly
`DOSSIER <slug> — <issue statement>`. A contribution or a delta MUST NOT start
with `DOSSIER <slug>` — title it `DOSSIER-CONTRIB <slug> — …` or
`DOSSIER-DELTA <slug> — …`. **And a contribution is never `kind="dossier"`.**
That kind is what `coord_recent_findings(kind="dossier")` lists as HEADS, so a
`DOSSIER-CONTRIB` posted with it reads as a second live head on the slug — the
accretion this step exists to stop (measured 2026-09-12: finding `0cda45a8`,
occurrence 19, listed beside the head `e1108168` until it was folded). Post a
contribution as `kind="gotcha"` (or `"status"`), with the slug in `topic`, and
let the next head merge it; only the merged head carries `kind="dossier"`.
Measured 2026-09-02: full-text search on the
literal key `DOSSIER coord-memory-search-zero-hit` DID return the head — at
rank 4 of 4, behind a `DELTA on dossier …` row and two `DOSSIER
plan-library-capture-loop … (contribution)` rows whose titles also began
`DOSSIER`. FTS finds the key; it cannot tell the head from its contributions,
and under the 50-hit cap a busy slug can page the head out entirely. The
prefix rule is what makes the head the only row whose title starts with
`DOSSIER <slug>`.

**Interim lookup rule (until `title_prefix` lands).** `coord_memory_search` is
**full-text only** unless the call carries a query vector — read its
`vector_arm` (`skipped_no_embedding` is the normal case) — so run **at least
three differently-phrased probes, one of them the literal key above**, and
treat **every empty answer as UNKNOWN, never as "no dossier"**. Read every coord/web/runner response through `scripts/lib/envelope.py` / `envelope.sh`; assert `count`-vs-rows agreement before acting on any zero; an `UNKNOWN:` line is UNKNOWN, not a negative.
The discriminating keys (`live_row_count` from `coord_memory_overview`,
`vector_arm`) are named in the helper's docstring, not here — the dossier
`0585cd3f` records why one probe is never enough.

**Resolution path once it lands.** Plan
`2026-09-02-steering-layers-unreadable-without-a-credential` Phase 2 adds a
server-side `title_prefix` argument to `coord_memory_search` (qontinui-web
first, then coord — a coord that sends it to an older web 422s). Head
resolution then becomes one call: `coord_memory_search(query_text="DOSSIER
<slug>", kinds=["mental_model"], title_prefix="DOSSIER <slug> —")`, newest
`created_at` first among live rows. Until the tool advertises the argument,
an unknown-argument call is rejected — fall back to the interim rule above.

**The supersede half is delivered elsewhere.** `coord_memory_supersede` — the
door that lets a head be corrected in place — is owned by
`2026-09-01-coord-memory-has-no-supersede-door-so-a-stale-record-cannot-be-corrected`
(coord#1845 + runner#1299; it reaches a box after the next runner rebuild, the
allowlist being compiled in). This convention is the LOOKUP half only, and it
governs **memory-resident** heads; a head that is already a finding is
superseded with `coord_post_finding(supersedes=…)`, which is live on every box
today — see the update rule below.

The first of those probes has the HTTP twin and the `available` bit described under
"Probe before you re-derive" (`GET /coord/agent-findings?topic=dossier:<slug>&kind=dossier`);
`available: false` here means you do not know whether a dossier exists, which is
not the same as knowing there is none. Read it through the envelope helper — its
docstring names this door's order, `available` before the collection — and treat
**every empty answer — from either store — as UNKNOWN, never as "no dossier"**: a
`count: 0` from the finding probe says only that no *migrated* head exists, so
the memory probes still run.

The memory probes carry the same caveat, from a different cause. A
`coord_memory_search` miss is UNKNOWN, not "no prior dossier", **specifically
because the file fallback in 2c produces records the search cannot see**: a
dossier authored into a local topic file never reached coord (Phase 3b retired
the sync), so the exact queries above return 0 for it however many occurrences
it records. Measured 2026-09-01: 0 hits on three phrasings for
`empty-read-published-as-verdict`, a dossier with 8 recorded occurrences that
existed only as a local file until that session ported it by hand. The
self-describing zero (`live_row_count`, `query_echo`, `anchored_hit_count` —
`CLAUDE.md`, "Recall") still holds and is still worth reading: it proves the
store was read and the query was the one you meant. It cannot speak for a
record that was never written to the store. Say both: "0 hits against a live
store; a fallback-written head may exist that coord cannot see". Before
opening a new dossier, grep this box's project memory dirs for `DOSSIER <slug>`
and for `coord_fallback:` — a hit is a head to port over the live door (2c)
and then UPDATE, never a second head.

**Two or more prior records of the same condition → open or update a dossier.**
Still file the finding — it is this occurrence's evidence — and add its
`finding_id` to the dossier's ledger.

**Primary shape.** A dossier is a coord **finding** of `kind="dossier"` — the
one finding kind that never expires — written and superseded with
`coord_post_finding` (HTTP twin `POST /coord/agent-findings`, the same cascade
as 2b → Transport):

```
kind          = "dossier"
topic         = "dossier:<slug>"          # a TAG, matched by exact string equality — spell the slug once
scope         = "fleet-infra" | "tenant"
title         = "DOSSIER <slug> — <one-line issue statement>"   # the deterministic key above, unchanged
resource_keys = the paths/plans a peer would be working when they hit it
supersedes    = <the prior head's finding_id>   # omitted only on a slug's first head
dossier_slug  = "<slug>"                   # top-level, as in 2b; coord stores it
                                           # at `artifact_refs.dossier_slug`, so the
                                           # ledger below carries it without you
                                           # spelling it twice. COLON-FREE
                                           # (`^[a-z0-9][a-z0-9-]*$`, 2-64 bytes) —
                                           # the `topic` grammar minus its `:`
                                           # suffix, because the `dossier:` above
                                           # already spends it; blank is refused,
                                           # absent is fine
artifact_refs = {                          # the STRUCTURED ledger (plan §4.1)
  "recurrence_count": <N>,
  "first_seen":       "<YYYY-MM-DD>",
  "last_seen":        "<YYYY-MM-DD>",
  "readiness":        "accumulating" | "ready_for_pvi" | "in_remediation" | "closed",
  "evidence":         { "memories": [<ids>], "findings": [<ids>] },
  "remediations":     [ { "plan": "<stem>", "status": "<that plan's status>", "held": false } ],
  "exit_criterion":   "<what would close it>"
}
body          = the synthesis: the issue; why it keeps winning; the evidence
                ledger, one row per line in the format below; the analysis;
                candidate directions for /pvi; and the explicit exit criterion
```

`artifact_refs` is the structured ledger and `body` is the prose one — write
**both, always**. `artifact_refs` is already in the findings read projection,
so *"which issues recur most despite shipped fixes?"* is answerable from
`coord_recent_findings(kind="dossier")` — every live head, client-sorted on
`recurrence_count` — without opening a body; and the `evidence` arrays carry
ids while the prose rows carry claims, so the day-15 expiry of a cited
finding loses a pointer, never the claim. `topic` has **no uniqueness
constraint** (plan §4.1): nothing in coord stops two sessions spelling one
slug two ways, so the deterministic title key and the lookup rule above are
the whole of the integrity — find the head first, then write.

**Fallback shape (a coord that rejects `kind: dossier`, or a head still in
memory).** A dossier is a coord **memory** record of kind
`mental_model`, titled exactly `DOSSIER <slug> — <one-line issue statement>`
(the deterministic key above), carrying: the issue; why it keeps winning; an
**evidence ledger**; the analysis; candidate directions for `/pvi`; and an
explicit exit criterion. Memory is permanent, agent-writable and full-text
searchable, which the plan corpus is not and which findings were not until
Phase 2 — so this is the shape every pre-Phase-2 head has, and the shape you
write when the serving coord refuses the kind. A contribution or delta in
this shape keeps its `DOSSIER-CONTRIB <slug> — …` / `DOSSIER-DELTA <slug> — …`
title, whichever shape the head is in.

**Every ledger row carries the claim, its date, and its source id — never a
bare id:**

```
- <YYYY-MM-DD> — <claim in one sentence> — finding <id> | memory <id> | plan <slug>
```

A ledger of bare ids rots: every finding kind but `dossier` — which is to say
every evidence row the ledger cites — expires at a hardcoded 14 days
(`FINDINGS_TTL_DEFAULT`, `findings.rs`), `recent()` filters `expires_at >
now()` unconditionally, and there is no UPDATE path — so on day 15 a row that
says only `finding 8b23391a` says nothing. The claim is what survives the
citation; the date is what lets a reader see recurrence without dereferencing
anything. The worked example is the live head
`60170b4c-e90a-4184-a09d-227758bc3a51` (`DOSSIER coord-merge-throughput — …`),
whose ledger already carries a claim per row and says why; add the date column
when that head is next superseded, not as a separate write.

> **Why a dedicated finding kind, and why not the plan corpus** — both stores
> were measured before the shape was chosen, and the history is what explains
> it. Until plan `2026-08-25-dossiers-durable-issue-files-for-recurring-defects`
> Phase 2, `coord.findings` expired at a **hardcoded 14 days no caller could
> override**, had **no UPDATE path** to extend it, and `recent()` filtered
> `expires_at > now()` unconditionally in its only SELECT — so a dossier stored
> there went **permanently unreachable on day 15**, and `kind` was not even a
> filter; that is why the first dossiers lived in memory. The `dossier` kind is
> **exempt from that TTL** (a per-kind interval at insert, no migration, no
> change to the read filter) and Phase 2 adds the **`kind` filter** to
> `recent()` and both doors — which is exactly what makes a finding the primary
> store above. The plan-library write door is closed to a degraded session too — but
> **not for the reason this line used to give.** It said the door "requires a web
> User principal"; it does not. `POST /api/v1/plan-library` and its edge routes
> take `get_audit_actor_user`, which has accepted a coord **device** JWT since
> 2026-08-15 (`704d394d`), attributing the row to the device's paired operator;
> the one Cognito-only route left on that surface is `PATCH /{id}/kind`, and its
> own comment says so. What actually closes it is narrower and worse: the
> `/agents/allocate` token that arm 2 **used to** send you to is minted by
> `jwt.rs` `issue()`, which sets **`user_id: None`**, and the web door refuses a
> device token with no `user_id` claim — `401 "Device token missing user_id
> claim."` (Arm 2 no longer sends you there at all: that rung is prohibited —
> see the ⛔ bullet above. This paragraph is kept because the `user_id` fact is
> still the reason the plan-library door is closed to a degraded session, and
> because a reader who finds the allocate route elsewhere should know it would
> not have opened this door either.) So the credential a *degraded* session
> could still mint is precisely the one that door rejects, while the healthy
> device JWT it no longer has would have worked. The measurements are plan
> `2026-08-25-dossiers-durable-issue-files-for-recurring-defects` §3 and §4.

**When updating, MERGE the synthesis forward and supersede the prior head. Do
not append a second dossier for the same slug.** Accretion without synthesis is
the exact failure this step exists to fix — it is how 8 memories on one issue
came to exist without anyone drawing the conclusion. Locate the head by its
key first (the lookup rule above); a contribution you cannot merge yet is
titled `DOSSIER-CONTRIB <slug> — …` so it never impersonates the head.

**The mechanism.** Post a **new** head with the merged synthesis and
`supersedes=<the prior head's finding_id>`; `recent()` returns only the live
head of a `supersedes` chain (`NOT EXISTS (… s.supersedes = f.finding_id)`, in
SQL), so the next `coord_recent_findings(topic="dossier:<slug>",
kind="dossier")` yields exactly the new head and the old one stays as history.
Bump `recurrence_count`, move `last_seen`, add this occurrence's `finding_id`
to `artifact_refs.evidence.findings` **and** as a dated prose row in the body.
Never re-post the old body with a line appended — merge.

**A head still in memory is migrated on this update, not left where it is.**
Post the merged synthesis as the slug's **first finding head** (primary shape,
no `supersedes`), then retire the memory rows so they stop answering as heads
— the plan's Phase 4 step 2 cascade, over the first door that answers:
(a) `coord_memory_supersede` if it is a visible tool; (b) qontinui-web's
device-JWT door — `POST https://api.qontinui.io/api/v1/memory/records/{id}/supersede`
for the newest head (it MINTS a replacement row, so give it a one-line pointer
titled `DOSSIER <slug> — MOVED to finding <finding_id>`) and
`DELETE https://api.qontinui.io/api/v1/memory/records/{id}` for every other
row on the slug; (c) if no legitimate credential reaches either door on this
box, **leave the rows in place and report the retire as OWED, with their ids**
— the finding is now the first probe, so an unretired memory head is stale
but no longer authoritative. **Never mint a credential from
`/agents/allocate` for this** (the ⛔ bullet under 2b; finding `65d574cc`).
If the serving coord rejected `kind: dossier`, there is nothing to migrate
into: supersede in the fallback shape — a new memory head, titled by the key,
whose body names the prior head's id as superseded — and report that the
finding head is owed to the first session that reaches a Phase-2 coord.

**Where a plan already addresses the issue class, add a `Dossier:` pointer to
that plan's status block**, so a session arriving via the plan finds the
accumulated evidence instead of re-deriving it. A dossier whose issue has NO
plan and reads `ready_for_pvi` is a `/pvi` candidate — carry it into Step 3 as
deferred work (`/pvi dossier:<slug>` takes the head directly), and note that a
SHIPPED remediation which did not stop recurrence is the most valuable input to
that plan, never a reason to skip it.

### 2c — Is it a durable fact, correction, or hazard? → the memory store

If the item is knowledge rather than work — a non-obvious property of the
system, a trap that cost this session time, a correction to a belief this session
started with, an approach that was confirmed — record it.

- When **`coord_memory_record` is visible and answers**, author through it.
  Kind mapping: `feedback`→`feedback`, `reference`→`reference`,
  user-fact→`fact`, project-state→`observation`. Redaction, dedup and quotas
  are enforced server-side, so pre-filtering beyond normal secret hygiene is
  redundant.
- When it is **masked**, or answers `"Command failed with no output"`, do NOT
  fall back to a file on that evidence. Visibility is not the predicate;
  **reachability** is, and one resolver decides it. Four steps:
  1. `bash <workspace-root>/qontinui-claude-config/.claude/skills/coord-revive/coord-revive.sh`
     from the real cwd, SUBSTITUTING the real workspace root — the ONE door
     resolver (its L2 is the same sibling sweep `/gate` Step 2 runs). Spell
     it absolutely: the relative `.claude/skills/...` form resolves only from
     a checkout that has that tree, which most agent worktrees do not. Note
     that an unsubstituted `<workspace-root>` produces `No such file or
     directory` too, so read your own paste error before reading a missing
     door. Branch on its `VERDICT:` line.
     Never paste the sweep into this file: the cascade's only implementations
     are the two scripts `lint-shared-door-classifier.py` (check #35) pins.
     **A correctly-substituted path that STILL says `No such file or
     directory` means no `qontinui-claude-config` checkout exists on this
     box at all** — steps 2
     and 3 below need the `VERDICT:` line this step produces, so neither is
     reachable either. That is functionally the same operational state as
     step 4's `DEAD`, so treat it as `DEAD` (skip straight to step 4) rather
     than as a masked-tool retry.
  2. **LIVE loopback proxy (L1/L2)** → raw JSON-RPC `tools/call`
     `{"name":"coord_memory_record","arguments":{title,content,kind}}`:
     against the `url=` that the `VERDICT: LIVE door=<file> url=<url>` line
     names, with that file's nonce, using the recipe `gate.md` Part B Step 2
     uses for `coord_register_gate` (nonce staged in a tempfile, `-H @file`,
     never on argv). **`coord-revive.sh` DOES have `call` and `tools` verbs**
     — and `coord-revive.sh --help` prints them, which settles it without
     trusting any document, this one included. So the hand recipe is A path,
     not the only one. What bounds the
     verbs: they carry **no cascade of their own**, they **never mint** a
     nonce, and they execute over the nearest proxy-shaped `.mcp.json` at or
     above `$PWD` — which is NOT always the door the cascade resolved
     (measured 2026-09-12 from a checkout whose own key was evicted: the
     cascade reached the root door while `call` kept taking the nearer dead
     one). So they are worth TRYING and are not a substitute for the VERDICT
     line: use whichever of the two actually carries the call.
     **Then verify by READ-BACK, whichever door you used.** No exit code and
     no stderr line says the WRITE landed: a tool that refuses — a bad `kind`
     from the map above, a missing required argument — comes back as
     `isError: true` inside a SUCCESSFUL envelope, so `coord-revive.sh call`
     prints `OK over` and exits 0 over a refused write (measured 2026-09-12).
     **The refusal text is on stdout: read it.** It names what was wrong, so
     fix the payload and re-issue — re-sending an identical call that your own
     arguments got refused only reproduces the refusal.
     Then read back with `coord_memory_search` over the same door
     (`query_text` is required there too — omit it and the search is refused
     the same way, `OK over` and all). A zero-hit search is self-describing:
     read `live_row_count`, `query_echo` and `anchored_hit_count` before
     concluding anything, exactly as 2b-bis requires. Only a CORRECTED
     payload that is still refused, over a door that carries the call, is a
     coord defect — report that, and the memory **DROPPED** with it. A
     refusal you have not yet read is your payload, not coord's.
  3. **LIVE bearer, no proxy (L3/L4)** → the web memory API directly:
     `POST https://api.qontinui.io/api/v1/memory/records` with
     `{"records":[{title,content,kind}]}` and a FRESH device JWT
     (`get_memory_tenant` verifies coord-signed device JWTs; measured HTTP 200
     on 2026-09-02; `~/.qontinui/coord-device-jwt` lives ~4h — check `exp`
     first). Read back with `POST …/memory/query` (`query_text` is required).
  4. **`DEAD`** — and only then — a topic file plus a **one-line** `MEMORY.md`
     index entry (one target per line, as plain index hygiene), with
     frontmatter `metadata.coord_fallback: <YYYY-MM-DD> no-door` so a backfill
     can grep for it. That file is a **single-machine, single-project record
     with no path into coord**: the cross-account sync was retired (Phase 3b of
     `2026-07-26-claude-session-memory-cutover-to-coord`), so nothing carries
     it anywhere. It is readable on this box only, and invisible to every
     other session — including the `coord_memory_search` probes 2b-bis
     prescribes.
- **A memory written to a file while a door was reachable is RECORDED locally
  but DROPPED fleet-wide.** Report it as **DROPPED** in Step 4's classification
  table — the file is not the evidence RECORDED requires — and re-issue it over
  the live door before closing. Plan
  `2026-09-01-memory-fallback-writes-into-a-store-with-no-path-to-coord`.

What belongs here is the thing that was **non-obvious**. What does not: code
structure, git history, anything already in `CLAUDE.md` or the knowledge base,
and anything that only mattered inside this conversation.

Before writing, search for an existing record covering it and **update that**
rather than adding a duplicate. `coord_memory_search` is **full-text only**
unless the call carries a query vector — read `vector_arm` through the envelope
helper (`scripts/lib/envelope.py`; its docstring is where that key and every
other response key is named) rather than assuming, and phrase the query in the
target record's own words while it reads `skipped_no_embedding`. A missing
answer is **UNKNOWN, not "no memories"**.

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

> **The cheapest read comes first, and it answers the question directly.** Before
> any AWS call, ask coord: `coord_flag_states` (no arguments) returns every
> catalogued flag with `kind`, `effective_state`, `dark_arm` and
> `enabling_condition_met`. **The sweep this step describes IS the filter
> `effective_state == "off" && enabling_condition_met == true`**, pre-counted as
> `summary.off_but_enabling_condition_met`. Each hit is a capability whose
> delivery conjunction already holds and whose only remaining blocker is the env
> var — exactly "built but not activated", answered by a read instead of by
> inference. `dark_arm` then tells you whether the absence was ever justified:
> `last_resort` with a now-satisfied `enabling_condition` is a discharged
> justification, and `unjustified` means nobody recorded one at all.
>
> Three ways to misread it, all measured 2026-09-06:
> - **`summary.off` is not the number.** Filter `kind == "capability"` first —
>   `kind: "tunable"` entries are numeric override knobs whose `off` means *no
>   override in effect*. 19 of the 34 flags reading `off` were knobs.
> - **`enabling_condition_met` can be the string `"unknown"`**, meaning a conjunct
>   was unobservable. That is a blind spot to name, never a `false`
>   [policy: `verification-and-evidence` `unknown-must-not-render-as-a-default`].
> - **It evaluates against the SERVING build.** A capability enabled on
>   `origin/main` and dark in the serving binary is a DEPLOY GAP, not a dark
>   capability. Cross-check `coord_query_health` →
>   `surfaces[0].components.build` (`in_sync`, `serving_sha`) — which is the same
>   read the AWS block below performs, and is why that block stays: it settles
>   WHICH code the flag answer describes.
>
> `coord_flag_states` is not on the `device` principal's allow-set (it answers
> `tool_not_available_to_principal`); it is on the **agent** principal's, so use
> the token from `POST /agents/allocate` against
> `POST https://coord.qontinui.io/mcp`. The standing census is the `diagnostic`
> artifact `2026-09-06-coord-dark-capability-census` — 63 dark capabilities, 31
> with no justification the clause recognises, 17 outside the registry entirely.
> **The registry covers coord only**, so for any other service the AWS task-def
> read below is still the whole answer.

> **Read the deployed configuration, never the in-code comment.** Comments
> describing an arm as "shadow" or "dry-run" go stale the moment the flag is
> flipped in deploy config, and are a documented source of wrong conclusions in
> this fleet. Check the deploy task definition / environment of the **running**
> service, and prefer probing a **feature-marker field** on a live endpoint over
> trusting `git_sha` (served empty) or a `buildId` you have not mapped to a
> commit.
>
> For coord that read is two calls, and the region is the trap:
>
> ```
> aws ecs describe-services --cluster qontinui-staging --services coord \
>   --region us-east-1 --query 'services[0].taskDefinition' --output text
> aws ecs describe-task-definition --task-definition <that> --region us-east-1 \
>   --query 'taskDefinition.containerDefinitions[*].image' --output text
> ```
>
> The image tag is the serving sha; `containerDefinitions[*].environment` is
> where the flag actually lives. ⚠️ **The cluster is in `us-east-1`.** The fleet's
> SSM reads are `eu-central-1`, so the region is easy to carry over from a
> neighbouring runbook — and the wrong one returns `ClusterNotFoundException`,
> which reads like an outage rather than a wrong region and can send this step
> down a false "the service is down" path. The rule is per-service, not
> per-runbook: SSM `eu-central-1`; ECS and Cognito `us-east-1`. Same read, with
> the debounce evidence that motivates it, in
> `.claude/commands/cleanup-steward.md` → **"A shipped fix is not a serving fix"**
> and `.claude/commands/merge-train-steward.md` → the **"Honest bookkeeping"**
> bullet (a bullet, not a step — searching for a step of that name finds nothing).
>
> ⚠️ **A green `Deploy coord` run is not evidence the flag you are reading is the
> one serving** — that workflow's spacing gate debounces the rollout while still
> reporting `success`. Only the task-def read above settles it.

If the capability exists and is merely off, the deliverable is **not a plan** —
it is an activation, plus a gate if flipping it waits on an observable
precondition or an operator decision.

#### 3a's output is a DIAGNOSTIC ARTIFACT — write it, do not leave it in the transcript

This sweep is a **producer** of the `diagnostic` artifact class. Whenever it
runs a readiness or doctor read — `coord_fixer_arm_readiness`,
`coord_gate_doctor`, `capability-doctor.sh` — and the answer clears the bar,
write the answer to the plan library rather than only reporting it.
Full body contract, write door and slug convention:
`knowledge-base/qontinui-specific/diagnostic-artifacts.md`.

**The EMITTER is this command, never the tool.** `coord_fixer_arm_readiness` and
`coord_gate_doctor` are coord MCP tools and coord has no client to qontinui-web's
plan library; `capability-doctor.sh` is a shell script. Each already emits
analysis-shaped output with provenance and has no sink. The producer is the
session that CALLS them, writing the tool's own JSON — verbatim, with the tool
name and a UTC stamp — as the diagnostic's **Measured** block.

**The bar (all three, or write nothing):** the analysis answers a question
someone actually asked (3a's own *"is it already built but not activated?"*
counts); it rests on ≥1 measurement with a named probe; and its conclusion would
be non-obvious to a competent session starting fresh — typically because it
inverts the obvious remedy. A run that finds nothing dark restates what the
config plainly says: that is noise, and noise dilutes the corpus a future
session is meant to trust. Do not write one.

**Repetition is a version, not a row.** Upsert on the stable slug
`probe-<topic>-<YYYY-MM-DD>`, so a second sweep the same day appends a version
to one row instead of adding a second. `source_repo` is the constant
`qontinui-dev-notes/diagnostics`; `kind` is `diagnostic`;
`kind_is_heuristic: false`; `intent_refs` cites the served `success_metric/` or
`domain_spec/` the finding bears on, so its significance is INHERITED rather
than asserted.

**A dark capability additionally owes a `domain_spec` divergence entry** — that
is the one automatic cross-populate direction, and it exists because
`engineering-priorities` `capability-ships-enabled` makes a capability nobody
switched on a product ABSENCE, which is exactly what a gap analysis ranks. The
discriminator: *does this change what someone would BUILD, or only what someone
would FIX?* Build → cross-populate; fix → the diagnostic store only. The
procedure (notification first, then `replace`, degrading to a Proposed-divergence
block in the diagnostic on a server deny) is in the same page.

**Which door.** Prefer `POST http://127.0.0.1:9876/plan-library/artifacts`; on a
box whose runner forwards to an unreachable web base it answers **502** and the
live door is the direct `POST https://api.qontinui.io/api/v1/plan-library` with
a device JWT carrying `user_id`. **Verify by read, never by the 201** —
`GET …/plan-library?kind=diagnostic&work_unit_slug=<stem>`; `?q=` matches title
and body but NOT the slug. Say which door you wrote through. Worked instance:
`probe-plan-corpus-2026-09-06-write-door-on-merytshost`.

### 3b — Is it already PLANNED but not IMPLEMENTED?

Query the plan corpus. **The DB is authoritative for reads** — discovery, search
and selection resolve against `agent.work_artifacts` behind qontinui-web
(`/api/v1/plan-library/*`), not against a directory. `$QONTINUI_PLANS_DIR` is an
**authoring surface**, and being unset is a supported configuration, not an
error.

**Three doors, in this order** (plan `2026-08-27-plan-corpus-read-path-is-dark`
Phase 4). `http://127.0.0.1:8000` is on no rung: it is a per-box dev backend,
and whatever it answers is an observation about that process, never about the
corpus. Do not diagnose it here, and do not quote a cause for it from any
document — the cause of that box's local 404 has flipped repeatedly, and a
session that repeats a stale one forecloses the reader's search.

**Door 1 — the runner, no credential.** The runner attaches its own device JWT
server-side; the caller presents nothing.

```bash
curl -sS -w '\nHTTP %{http_code}\n' \
  'http://127.0.0.1:9876/plan-library/search?kind=plan&slug=<stem>'
# the body, on a runner build that carries the by-id forward (READ-PLAN Phase 2):
curl -sS 'http://127.0.0.1:9876/plan-library/artifacts/<id>'
```

A non-2xx names the host the runner dialled — its configured web base. Record
the status and that host as an observation about that base; it says nothing
about the corpus.

**Door 2 — git, no credential and no service.** Authoring layer only (a plan
authored through the web UI is invisible here), exact stem, `origin/main` as of
the last fetch:

```bash
git -C qontinui-dev-notes fetch -q origin main
bash qontinui-claude-config/scripts/lib/pinned-read.sh \
  --root qontinui-dev-notes cat origin/main plans/<stem>.md | head -5
git -C qontinui-dev-notes ls-tree --name-only origin/main plans/ | wc -l
```

⚠️ Read the pinned-read helper's EXIT CODE, not the emptiness of its output --
that is the whole point of using it here. `git show origin/main:plans/<stem>.md
| head -5` signals "no such plan on the ref" ONLY through exit 128, and the pipe
spends it, so a plan that was RENAMED and a plan that does not exist print the
same nothing. The helper answers `0` FOUND / `1` MISSING_AT_REF / `2` UNKNOWN,
and emits content on stdout only in state 0. A `2` here is UNKNOWN -- never
"this plan was never written".

**Door 3 — the deployed backend, with a coord DEVICE JWT.**
`~/.qontinui/coord-device-jwt` carries the `user_id` claim the route requires;
an `/agents/allocate` agent token does not. When the file token is expired,
mint one from the runner (it holds no secret at rest):

```bash
# DOOR 3a, always first: the runner's IN-PROCESS invoke mint - no WebView hop,
# so it answers on a headless runner too. The reply is the runner's ApiResponse
# envelope and `data` IS the token: {"success":true,"data":"<jwt>"}. Record
# which door answered (runner-invoke / runner-eval) beside the read - never the
# token.
# `data: null` from get_coord_device_token is "this device is unpaired" - a
# verdict, not a reason to try the next name. Only an HTTP 400 "not in UI Bridge
# allowlist" (or 404) moves on to the older name for the same slot.
curl -sS -X POST http://127.0.0.1:9876/ui-bridge/invoke/get_coord_device_token \
  -H 'Content-Type: application/json' -d '{}'      # -> .data (source: runner-invoke)
curl -sS -X POST http://127.0.0.1:9876/ui-bridge/invoke/get_access_token_for_websocket \
  -H 'Content-Type: application/json' -d '{}'      # only after the 400/404 above

# DOOR 3b, ONLY when 3a answered HTTP 400 "not in UI Bridge allowlist" (or 404
# for the route) for BOTH names - a runner build that predates both entries; a
# START of a newer build picks get_coord_device_token up, never restart a
# running one over it. This is the WebView eval
# mint: CSP-refused on the builds measured refusing (58414a05-1788118917383 on
# 2026-09-02; an unrecorded build on 2026-08-31 - it ANSWERED on
# 546e9e024-1788209530736, 2026-09-01: per-build, never every build), and it
# bounces through the WebView.
# RUNNER HEADLESS FIRST for THIS door: check /health `frontendReady` before
# spending a timeout on it. `frontendReady: false` means the eval mint is a DEAD
# TRANSPORT, never that you are signed out: a live credential can be sitting in
# the runner store the whole time. The `/coord-mcp` nonce mint (coord only)
# cannot carry this read either - so on a headless box without the invoke entry
# export COORD_DEVICE_JWT, or use Door 1 or Door 2, which need no credential
# at all. NOTE the eval token is at data.value, NOT data.result.value: the
# runner unwraps the frontend's `result` envelope before it reaches HTTP.
# Plans 2026-08-24-headless-box-has-no-working-coord-credential-door,
# 2026-09-02-steering-layers-unreadable-without-a-credential (1f).
curl -sS http://127.0.0.1:9876/health   # -> .data.frontendReady

curl -sS -X POST http://127.0.0.1:9876/ui-bridge/control/page/evaluate \
  -H 'Content-Type: application/json' \
  -d '{"expression":"window.__TAURI__ ? window.__TAURI__.core.invoke(\"get_access_token_for_websocket\") : invoke(\"get_access_token_for_websocket\")","await_promise":true}'   # (source: runner-eval)

# Then read the corpus, staging the bearer OFF argv into $AUTHFILE:
curl -sS --get 'https://api.qontinui.io/api/v1/plan-library' \
  --data-urlencode 'kind=plan' --data-urlencode 'slug=<stem>' -H @"$AUTHFILE"
```

Two independent sessions on 2026-08-25 both reported PLAN-STATUS UNKNOWN off a
localhost 404 alone while Door 3 was answering 200 with real rows — and Door 1
had been shipping, unprobed, the whole time.

**On every list result:** check that the returned `slug` equals the stem — a
backend predating the `slug` filter ignores the parameter and returns an
unfiltered page (`work_unit_slug=<stem>` is the older exact door, null for a
hand-`POST`ed row) — and read `corpus_health` (`artifact_count`, `plan_count`,
`newest_updated_at`, on a backend carrying READ-PLAN Phase 2). A `plan_count`
far below Door 2's `ls-tree` count is a FROZEN corpus; write that observation.

**The cache is the last rung, and only where a PowerShell interpreter exists**
(`pwsh` on Linux via `scripts/install-pwsh-linux.sh`; where none is present
`scripts/capability-doctor.sh` reports the renderer INOPERATIVE, and it is not a
degraded arm at all). `$QONTINUI_PLAN_CACHE_DIR` (default
`C:/claude/plan-corpus-cache/`, `${XDG_CACHE_HOME:-~/.cache}/qontinui/plan-corpus-cache/`
on Linux) — `PLANS-CACHE.md` for the index, `bodies/<kind>__<slug>.md` for
bodies. Refresh with `scripts/render-plan-cache.ps1 -MaxAgeHours 0`; it dials
the deployed backend unless `-ApiBase` or the runner's backend variables say
otherwise, and on every failure it leaves the previous render in place and
rewrites only the header's `Last attempt:` line.

Its sidecar `PLANS-CACHE.state.json` records what the last attempt SAW, never
what it meant: `render_exit_reason` is one of `http_<code>` (`http_404`,
`http_401`, ...), `transport_error`, `empty_response`, `no_credential`,
`runner_headless`, `mint_door_refused`, `mint_transport_failed`,
`skipped_fresh`, `write_failed` or `ok`, with `attempt_url` beside it — the URL
that answered — and, on a 404 against a loopback base, `checkout_probe`, the
shared checkout-staleness module's observation about this box's qontinui-web
checkout (witness present or absent at `HEAD`, the floor behind `origin/main`).
**Quote the reason and the URL you actually read; do not translate them into a
cause.** Read `api_base` as well: the last SUCCESSFUL render may have come from
a different host than the one it now retries.

**Say plainly which surface you read, and quote its `Rendered` stamp.** Read the
`PLANS-CACHE.state.json` sidecar: a `rendered_at` of `null` means the cache has
**never** rendered, and a stale or absent cache is **UNKNOWN, never empty**.
"This render did not see it" is not "it does not exist" — and reporting a missing
plan as absent is precisely how the same plan gets authored twice.

**A corpus that ANSWERS is not a corpus that is POPULATED.** The plan-library
body sync (`body_push.rs` -> `agent.work_artifacts`) is a property of each
writing device's runner build — opt-in under `QONTINUI_PLAN_LIBRARY_SYNC=1`
(`trigger.rs` `body_sync_enabled()`) on a build predating plan
`2026-09-03-plan-library-write-door-nonce-authorized-and-body-sync-on-by-default`
Phase 3, on by default after it — and gated again per cycle on the tenant's
`plan_capture` fleet dial. The operational layer (`coord.work_units`) fills
regardless, so the two layers can diverge with nothing logged on the older
build — measured 2026-08-22 on the operator box: **zero of the 343 plans
scanned** carried a row in `agent.work_artifacts`, while every work-unit row was
correct. That is one box's reading on one day, not the corpus's state. A `200`
carrying an empty list is therefore **UNKNOWN, not "no such plan"** until
`corpus_health` or the two counts above say otherwise — it is the
frozen-corpus signature, and it is the one path that reaches 3c **without any
door ever failing**, so the UNKNOWN clause below does not catch it on its own.
Record the count beside the zero.

**Do not probe by stem with `q`.** `GET /api/v1/plan-library?q=` matches **title and
body, NOT the slug** (measured 2026-08-22), while the stem is the canonical plan
identifier everywhere else - `Depends-On:`, the `Plan: <stem>` PR marker,
`$QONTINUI_PLANS_DIR` filenames. A by-stem `q` probe therefore returns a
**false negative for a plan that is present** - another definite-looking "no"
that routes straight to 3c. The exact door is `?kind=plan&slug=<stem>` (check
the returned `slug`), then `?kind=plan&work_unit_slug=<stem>` - the adapter
writes a plan's own stem into `work_unit_slug` (`body_push.rs`, `kind == Plan`
only). Failing both, page `?kind=plan&limit=200` and match the `slug` field
yourself; a zero from any of them is still UNKNOWN under the frozen-corpus rule
above.

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
5. **Dossiers touched** — for each: the slug, the head's `finding_id` (or its
   memory id, for a head still in the fallback shape — say which), whether it
   was OPENED or UPDATED, the recurrence count after this session's
   contribution, and the readiness verdict. A dossier that reached `ready_for_pvi` is the highest-value
   line in this report — it names a defect the fleet has already tried and
   failed to fix, with the evidence assembled.
6. **Residual DROPPED** — items you could not convert, each with the reason and
   the specific failure you observed. **Never report an empty residual list you
   did not verify.**
7. **Surfaces read, and their freshness** — which doors answered, which were
   degraded, and the stamp on any cache you relied on. A verdict computed from a
   stale cache is a stale verdict and must say so.
8. **Plan-corpus reachability - report this even when Step 3 never ran.** Quote
   `PLANS-CACHE.state.json`'s `render_exit_reason` and `rendered_at` as a
   one-line fact. Step 3 is gap-driven, so a clean session never looks at the
   corpus at all; this line is then the only thing standing between the fleet
   and a corpus that has been dead for days. A `rendered_at` of `null` means it
   has **never** rendered. Report it; do not investigate it here.
   Report the reason **verbatim**, with the sidecar's `attempt_url` beside it,
   and do not translate either into a cause: the field holds what the last
   attempt SAW — `http_<code>`, `transport_error`, `empty_response`,
   `no_credential`, `runner_headless`, `mint_door_refused`,
   `mint_transport_failed`, `skipped_fresh`, `write_failed` or `ok` — and a
   `checkout_probe` observation where a 404 came from a loopback base. **None**
   of those is evidence the plan library is unshipped, and rendering an
   `http_404` as "the route does not exist" is the absence-vs-unknown
   conflation this whole step exists to prevent. Quote whatever you actually
   read rather than forcing it into a vocabulary from this document.

### Honesty rules for the report

- **Absence is UNKNOWN, not zero.** Every unreachable door, silent-empty probe
  and unrendered cache is reported as UNKNOWN, with the failure named.
- **State the reach of every sweep.** A bounded or partial sweep never reads as a
  complete one.
- **A conversion is not done until it is read back.** Report what you verified,
  not what you issued.
- If this command's own procedure was the thing that failed, say so — and treat
  that as a capture gap in Step 3 like any other.

---

## Step 4.5 — report the continuation work outcome

If this session was spawned as a **coord gate continuation**, the gate is still
carrying whatever the runner wrote at spawn — `spawned` / `spawn_failed`, a
value stamped the instant the terminal appeared. It answers *"did a process
start?"*; it has never been able to answer *"did the work happen?"*. This
session is the only actor that can, and Step 1 has already computed the answer.

**Do not form a second judgement here.** The outcome is a projection of the Step
1 classification and nothing else — the same predicate Step 5's finish gate
uses, so the two writes can never disagree:

| Step 1 outcome | Outcome to post |
|---|---|
| every unit LANDED / WATCHED / RECORDED | `work_completed` |
| any unit IMPEDED or DROPPED | `work_abandoned`, detail = the reason |

A residual DROPPED item you could not convert counts as DROPPED here exactly as
it does in Step 5. An audit that classified everything as converted **because**
it wanted to post `work_completed` has inverted this step; the classification is
upstream and is not revisited.

**The precondition is two environment variables, and you read them BY NAME:**

```bash
GATE_ID="$(printenv QONTINUI_GATE_ID)"
GATE_DEVICE_ID="$(printenv QONTINUI_GATE_DEVICE_ID)"
```

Never an `env` dump — the session environment carries plaintext passwords, and
the habitual `JWT|KEY|TOKEN|SECRET` redaction filter matches no variable named
`PASSWORD`. The runner injects both **only for a genuine gate continuation**:
coord's payload slot is overloaded, and a work-unit DAG dispatch reuses the same
frame with a `dispatch_id` and no `coord.gates` row at all. **Either variable
absent means there is no gate to report to — skip this step silently.** Never
guess a gate id and never substitute a device id from another source: the
outcome UPDATE carries `AND continuation_consumed_by = $3`, so a device id that
is not the consuming one writes nothing and reports nothing. Absent variables
are "not a continuation", which is **not** the same as UNKNOWN and needs no
escalation.

**The call** — coord's unauthenticated device-keyed data-plane ack, so no bearer
(`$COORD_HTTP_URL` defaults to `https://coord.qontinui.io`):

```bash
# every unit LANDED / WATCHED / RECORDED
curl -sS -X POST \
  "$COORD_HTTP_URL/coord/gates/$GATE_ID/continuation-consumed" \
  -H 'Content-Type: application/json' \
  -d "{\"device_id\":\"$GATE_DEVICE_ID\",\"outcome\":\"work_completed\"}"

# any unit IMPEDED or DROPPED - detail is ONE line, naming the items
curl -sS -X POST \
  "$COORD_HTTP_URL/coord/gates/$GATE_ID/continuation-consumed" \
  -H 'Content-Type: application/json' \
  -d "{\"device_id\":\"$GATE_DEVICE_ID\",\"outcome\":\"work_abandoned\",\"detail\":\"<the IMPEDED/DROPPED items and why>\"}"
```

`work_completed` is persisted **bare** — coord carries no detail on it, so the
transition guard can compare it exactly; sending one is discarded.
`work_abandoned` is persisted as `work_abandoned: <first line of detail,
trimmed to at most 200 chars>`, so spend that line on the items themselves, e.g.
`2 DROPPED: page-health probe unconvertible (findings door 503), spec update`.

**Read the response — the 200 is not the answer, `outcome_recorded` is.** coord
echoes what it actually persisted. Compare `work_completed` by equality and
`work_abandoned` by **prefix**, never equality on the bare marker. A non-2xx, a
missing `outcome_recorded`, or a value that is not the one you sent is **a
failure to report** — this command's own honesty rules apply to it unchanged: a
write is not done until it is read back, and a producer that reads a 200 as
success is the silent-success defect this route was fixed to expose. coord
permits exactly one `spawned → work_*` transition and refuses `work_* → work_*`,
so a refusal means an outcome already stands; name the value that stands rather
than claiming yours landed.

**Best-effort, never blocking, and never a substitute for Step 5.** A missing
variable, an unreachable coord or a refused write never blocks the report, the
conversions, or the finish write. Add one line to the Step 4 report:

> **Continuation outcome** — gate `<gate_id>`, `outcome_recorded: <value>`.

or, when it did not land:

> **Continuation outcome NOT reported** — `<the failure you actually saw>`.

or, when both variables were absent:

> **Continuation outcome** — not applicable; this session is not a gate
> continuation.

---

## Step 5 — record the session as finished

The audit is done and reported. This step writes the one durable fact that makes
the report matter operationally: **whether anyone needs to come back here.**

A finished session is dropped from the runner's resume set, so after the next
runner rebuild it does not reappear among the sessions asking to be resumed.
That is the whole point — an operator rebuilding a runner should be handed the
work that is still open, not everything that ever ran.

### The gate

**Mark the session finished UNLESS Step 1 classified any unit as IMPEDED or
DROPPED.**

Those two are exactly the states that mean a human must return. LANDED, WATCHED
and RECORDED are all "this is in a durable store and will be picked up without
me"; IMPEDED and DROPPED are not. So:

| Step 1 outcome | Action |
|---|---|
| every unit LANDED / WATCHED / RECORDED | **finish** the session |
| any unit IMPEDED or DROPPED | **leave it unfinished**, and say which items held it open |

A residual DROPPED item you could not convert (Step 2) counts as DROPPED here —
converting it is what would have cleared the gate, and reporting it as converted
when it was not is the failure Step 4's honesty rules already forbid.

### How

> ⚠️ **FIRST: are you a SUBAGENT? If so, do NOT run `/finish-session` here.**
>
> A Claude Code subagent inherits its parent's `$CLAUDE_CODE_SESSION_ID` — it is
> a context inside one harness session, not a session of its own — so the finish
> below lands on the **parent**, which is very likely still working. `finished`
> trips coord's universal prompting gate (`qontinui-coord` `sessions.rs`,
> `prompting_allowed_decision`), so it silences every coord-originated prompt
> into that live parent while returning success to you.
>
> Instead: render the closeout as its own row in the Step 4 final-state table —
>
> > **Session closeout — OWED BY THE PARENT.** This `/unattended` ran in a
> > subagent context under session `<the inherited id>`; finishing that id would
> > finish the parent. The parent's own `/unattended` owns this step.
>
> — and continue. Everything else in Step 5 is skipped, not failed. The two
> contexts are **not separable by any mechanical signal** (measured 2026-09-04:
> identical environment, identical pid, no per-context marker), so you are the
> only thing that knows; `/finish-session` Part A step 0 carries the same arm and
> its step 0.5 is the identity-free backstop. Plan
> `2026-09-04-closeout-commands-have-no-subagent-arm-and-finish-their-parent`.

Run **`/finish-session`** with a reason naming the audit result, e.g.
`--reason "unattended: 7 units, all landed"`. It runs the transport cascade and
handles the id resolution.

If you write it directly instead, **pass `claude_code_session_id` explicitly**,
and pass **your own** — not whatever `~/.qontinui/agent_session_id` happens to
hold, which is box-global and has twice been measured pointing at a live peer.
An unscoped call is refused outright (`harness_session_id_required`); there is no
device-wide fallback left to reach for, and there must not be — falling back to
the device-wide pick would mark somebody else's session done and remove it from
their resume set.

**When that scoped write is refused, read WHICH refusal you got.** Since
`qontinui-coord#2052` there are two strings with two different remedies:
`caller_owns_no_active_session` (absence — *"Register one with `POST /sessions`
and re-report"*) and `session_not_owned_by_caller` (you own rows, this id
resolved to none — *"a row whose `claude_code_session_id` is still NULL is
invisible to this resolver, and is repaired by
`POST /coord/sessions/bind-harness-session`"*). The second no longer means "owns
no row for this device"; that gloss predates `#2052`. Both are expanded, with the
terminal state to report, in the two-failure split at the end of this step.

### What this step is NOT

- **It does not close the session.** Finishing is metadata; the session keeps
  running until it exits on its own. This command must never terminate the
  session it is reporting from.
- **It is not a verdict on the work.** `/unattended`'s verdict is COMPLETE /
  INCOMPLETE and lives in Step 4. This records only whether anyone must return.
- **It is reversible.** `/finish-session --undo`.

### Reporting it

Add one line under the Step 4 verdict:

> **Session marked finished** — carried by <rung>, read back <where>.

or, when the gate held:

> **Session left UNFINISHED** — <N> IMPEDED / <M> DROPPED: <the items>.

Both are complete outcomes. A session left unfinished because work genuinely
remains is this step working, not failing.

**Bookkeeping, so it never blocks.** This write is visibility, not correctness.
When it cannot be made, record that it was skipped, say why, and finish the
report. Do not raise a credential escalation for it. But do not claim it either:
a session reported finished on the strength of an unverified write is exactly
the false-completeness this whole command exists to prevent.

⚠️ **"Every rung probed and unavailable" is not one failure — say which of two
you hit.** `/finish-session`'s cascade is **two** rungs, not three (the
runner-local `POST /sessions/<id>/finish` was deleted 2026-09-05: it existed on
no runner build, upstream included — plan
`2026-09-03-a-finished-session-cannot-record-that-it-finished`). Both remaining
rungs carry the SAME tool to the SAME server over different transports, so a
cascade that ends without a write means one of two things, with different
remedies and different write-ups:

- **A transport floor** — no door answered. Run `/coord-revive`, re-issue over
  the door it reports LIVE, and treat a `"Command failed with no output"` write
  as presumed **LOST**. Only after that is the floor real, and only then is
  "exhausted cascade" the honest phrase.
- **An authorization verdict** — a door ANSWERED and refused. Retrying is
  pointless (`coordination` `briefing-mandated-door-that-answers-disabled`), and
  dropping the session id to make it succeed would write a PEER's row — served
  policy `coordination` `report-status-must-name-its-own-session` refuses it for
  that reason. **Since `qontinui-coord#2052` this is TWO reason strings with two
  different remedies; name which one you got.** Quote coord's own hint rather
  than paraphrasing it — the wording carries the remedy:

  - **`caller_owns_no_active_session`** — absence. Your device + tenant own **no**
    active `coord.sessions` row at all, so nothing here is a permission problem.
    Coord's own hint: *"this is ABSENCE, not an authorization violation: there is
    no peer's row you reached for … The usual cause is a session that was not
    runner-spawned, so coord was never told it exists. **Register one with
    `POST /sessions` and re-report.** Do NOT retry without the argument."*
  - **`session_not_owned_by_caller`** — you own rows; this id resolved to none of
    them. Coord's own hint: *"If you DO own an active session, check whether its
    row is simply UNBOUND before assuming you mistyped the id: a row whose
    `claude_code_session_id` is still NULL is invisible to this resolver, and is
    **repaired by `POST /coord/sessions/bind-harness-session`** — not by
    re-sending a different UUID."*

  ⚠️ **This string no longer means "owns no row for this device."** That was true
  before `#2052` (landed `3014d7c6`, 2026-09-09) split absence out, and it is the
  gloss this file used to carry. A build predating `#2052` cannot emit
  `caller_owns_no_active_session` at all, so on such a build
  `session_not_owned_by_caller` carries **no** diagnostic weight and the split
  above does not apply — check the serving build before reading it as "owns rows".

**On the authorization verdict, record the honest terminal state — this is a
CHECKED step, not prose.** When the refusal is `session_not_owned_by_caller` and
the id you sent was your own `$CLAUDE_CODE_SESSION_ID`, the outcome line is:

> **Session NOT finished — owns rows, none bound.** `<session id>` on device
> `<device id>`; coord answered `session_not_owned_by_caller`. The remedy
> (`POST /coord/sessions/bind-harness-session`) needs the `coord_session_id`
> this caller cannot learn. Dossier
> `finish-session-no-safe-rung-for-unregistered-session`.

Record it on that dossier as the next occurrence — a finding ON the dossier, not
a fresh unattached one; this condition has recurred a dozen times across two
devices and an unattached finding starts the count over. Quote the reason string,
the serving build id, and the transport that carried the call.

⛔ **Do not reach for `~/.qontinui/agent_session_id` to make the write succeed.**
It is box-global and has twice resolved to a LIVE peer; `/finish-session` step 1a
refuses it for exactly this reason, and finishing a peer silences every
coord-originated prompt into it while returning success to you.

Writing the authorization verdict up as an exhausted transport cascade hides a diagnosable
defect behind a transport excuse, and it is the reading that let this condition
recur across two devices before anyone planned it.
