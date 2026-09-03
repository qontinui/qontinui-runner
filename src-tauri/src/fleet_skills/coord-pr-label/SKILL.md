---
name: coord-pr-label
description: Set coord:* labels on a pull request — declare intent (upstream-of/downstream-of/stacked-on dependency edges, requires-tag, merge-strategy, blocked/experimental flags) so the PR Merge Orchestrator can schedule the auto-merge correctly. All three dep labels work cross-repo with the [<owner>/]<repo>#<n> grammar; no label holds a PR. Validates against the namespace and GitHub's 50-character label-name ceiling before sending (--dry-run checks a label without sending anything, and a failing gh pr edit is diagnosed rather than relayed); tenant resolved automatically from your agent's worktree.
user-invocable: true
---

# coord-pr-label

Set `coord:*` labels on a pull request to express PR-merge-orchestrator
intent. Wraps:

1. `gh api -X POST repos/<owner>/<name>/issues/<pr>/labels` — GitHub-side
   label add (canonical state). Not `gh pr edit --add-label`: that path
   prefetches the PR over GraphQL selecting the retired `projectCards` field
   and exits 1 on gh 2.46.0 before touching the label (measured 2026-09-03).
   The REST route also creates a missing dynamic-value label on the fly.
2. `POST <coord>/pr-merge/labels` — coord-side ingest hook that records
   the label in `coord.pr_labels` with `source='coord_skill'` + tenant
   scoping resolved from your `agent_id`.

The skill validates the label against the namespace before either call
fires, so invalid labels never make it to GitHub or coord.

## When To Use

- **Declaring a dependency**: all three dep labels share one value
  grammar — `[<owner>/]<repo>#<n>` (`stacked-on` also keeps bare `#<n>`
  for same-repo). Only the must-land-second side of an edge waits:
  - "this PR must merge after qontinui/qontinui-schemas#42" →
    `coord:downstream-of=qontinui/qontinui-schemas#42` (this PR waits;
    #42 unaffected).
  - "that PR must merge after this one" →
    `coord:upstream-of=qontinui/qontinui-web#99` (the carrier does NOT
    wait; #99 waits until this PR lands).
  - **Stacking a PR on a parent**: `coord:stacked-on=#42` (same repo) or
    `coord:stacked-on=qontinui/qontinui-web#42` (cross-repo) — this PR
    waits until the parent lands.
    **(code stacks only — see the migration callout below).**

  Labels on the waiting side are auto-stripped when the parent lands;
  two green PRs with a declared edge land in dependency order,
  unattended.
  ℹ️ **The coord-landed-parent hole is CLOSED** (2026-08-07). It used to be
  real: an ff-land closes the parent without ever producing the close cause the
  strip was keyed on. On the **sha-rewriting** shape GitHub emits no merge event
  at all; on the **sha-preserving** shape it does, but the webhook path stamps
  `close_cause = 'merged'`, never `commits_landed_via_other_pr` — so the
  webhook-gated strip never fired on **either** shape, and `downstream-of` is
  invisible to the edge table besides. The child kept a satisfied label and sat
  `CLEAN` and unproposed, runner#801 for 7 days. Which shape runner#801 ran on
  was not determined, and it does not matter — **do not rule out a recurrence on
  shape grounds** (`knowledge-base/qontinui-specific/coord-merge-train.md`; the
  two-shape model is `coord-ff-lands.md`). Two triggers close it now: a strip
  hook on coord's own land path, plus a reconciler sweep for any pre-existing
  backlog. **If you see it recur, do not diagnose it from
  `repo_branches.close_cause`** — it is sticky (nothing in coord ever clears it)
  and it is stamped by non-webhook writers regardless, so a post-hoc read shows
  *a land cause* — `commits_landed_via_other_pr`, or `merged` where the webhook
  won the first-writer race on the sha-preserving shape — **whether or not the
  strip trigger ever fired**; either way the row cannot tell the two apart. Read
  the reconciler metrics in
  the order `coord-merge-train.md` gives instead. Say so, because a recurrence
  now is a defect in one of the triggers rather than the known hole. Detail:
  `knowledge-base/qontinui-specific/coord-merge-train.md`.
- **Pinning a required tag**: `coord:requires-tag=ts-v*`.

> **No label holds a PR** (holds retired 2026-06-20). To hold a PR:
> **convert it to draft**, or **register a coord gate with a `MergePr`
> continuation**. `coord:blocked` / `coord:experimental` are still
> accepted — they route dequeue-time merge-class as flag labels — but
> they do NOT block merge; applying one via GitHub triggers a one-time
> PR comment stating it is inert as a hold. `coord:operator-review` and
> `coord:version-bump=*` are retired outright and REJECTED by the
> validator.

> **Do NOT use `coord:stacked-on` / `coord:upstream-of` /
> `coord:downstream-of` to order a *migration* stack.** When a PR's
> alembic migration must land after a sibling's, coord ALREADY derives
> that ordering from the `down_revision` chain — it emits an
> `EdgeKind::StackedOn` serialization edge on its own
> (`qontinui-coord/crates/coord/src/pr_merge/dep_graph.rs` `predict_migration_stacks`),
> with no label required. A hand-added label is redundant. Just author
> the migration with `down_revision` = your local alembic head and push —
> the chain IS the order. Reserve the dep labels for genuine **code**
> stacks: one PR's source depends on another's, with no shared migration.

**Don't use** to set `coord:state=*`, `coord:blocked-by=*`, or
`coord:specialist-decision=*` — those are coord-set (read-only via
this skill). The skill rejects them with a clear error.

See `<workspace-root>/qontinui-dev-notes/docs/coord/pr-merge-labels.md`
for the full namespace + semantics + conflict-resolution rules (if
`<workspace-root>/qontinui-dev-notes` is not checked out, skip the
reference — the summary above is sufficient).

## Inputs

- **PR number** (required) — `<n>`, e.g. `42`.
- **Repo** (required) — `<owner>/<name>`, e.g. `qontinui/qontinui-coord`.
- **Label** (required) — full label string, e.g.
  `coord:upstream-of=qontinui/qontinui-schemas#42`.
- **Agent ID** (resolved automatically) — the skill reads
  `$QONTINUI_AGENT_ID` from the environment. This is set by the
  agent-spawn flow; if absent the skill exits with an explanation.
- **Coord URL** — defaults to `http://localhost:9870`; override via
  `$COORD_URL`.
- **`--dry-run`** (optional) — validate the label (namespace grammar +
  GitHub's 50-character ceiling) and exit without touching GitHub or coord.
  `$QONTINUI_AGENT_ID` is not required for a dry run. Because it sends nothing
  it **cannot check whether the label exists**, which is the other cause of
  `'<label>' not found`; the report names that gap rather than letting
  `is valid` imply it was closed.

## How To Use

`set-label.sh` sits next to this SKILL.md, so every invocation below spells its
path relative to THIS SKILL DIR — `<path-to-this-skill-dir>/set-label.sh` — and
never through a `qontinui-claude-config` checkout. The skill is delivered by
being copied into `<session-workdir>/.claude/skills/coord-pr-label/`, on devices
that have no such checkout and in worktrees that have no such subtree, so a
config-repo path is a step that resolves in the operator's tree and fails
everywhere else.

### Set an upstream dependency

```bash
QONTINUI_AGENT_ID=<uuid> \
  bash <path-to-this-skill-dir>/set-label.sh \
  --repo qontinui/qontinui-coord \
  --pr 75 \
  --label "coord:upstream-of=qontinui/qontinui-schemas#42"
```

### Stack a PR on a cross-repo parent

```bash
QONTINUI_AGENT_ID=<uuid> \
  bash <path-to-this-skill-dir>/set-label.sh \
  --repo qontinui/qontinui-coord \
  --pr 75 \
  --label "coord:stacked-on=qontinui/qontinui-web#748"
```

### Set merge strategy

```bash
QONTINUI_AGENT_ID=<uuid> \
  bash <path-to-this-skill-dir>/set-label.sh \
  --repo qontinui/qontinui-coord \
  --pr 75 \
  --label "coord:merge-strategy=squash"
```

### Check a label without sending it

```bash
bash <path-to-this-skill-dir>/set-label.sh \
  --repo qontinui/qontinui-coord \
  --pr 75 \
  --label "coord:downstream-of=qontinui/qontinui-dev-notes#1234" \
  --dry-run
```

```
error: label is 52 characters; GitHub caps a label name at 50
       "coord:downstream-of=qontinui/qontinui-dev-notes#1234"
       drop the owner -- coord restores it via coord.tenant_repos:
         --label "coord:downstream-of=qontinui-dev-notes#1234"   (43 chars)
       NOTE: gh reports this as "'<label>' not found", which is NOT a
       missing-label problem -- gh label create cannot succeed either.
```

The short form is only offered when it is a label the validator itself accepts
**and** it still names the same repo — the owner has to match `--repo`'s owner,
because coord canonicalizes a bare name to the *tenant's* owner. For a foreign
owner, or anything else with no safe shortening, the error says
`shorten the value` and suggests nothing rather than handing you a label that
would be rejected or would point somewhere else.

## Validation Rules

The skill enforces the namespace before sending — this table mirrors
`labels_routes.rs::validate_label` exactly:

| Label                                              | Validation                                                 |
|----------------------------------------------------|------------------------------------------------------------|
| `coord:upstream-of=[<owner>/]<repo>#<n>`           | Contains `#`; repo part non-empty and — when it carries a `/` — **both segments non-empty** (`/repo`, `owner/` rejected); `<n>` parses as a Rust **`i32`**, so `#2147483648` overflows and is rejected while `#-1` and `#+1` are accepted |
| `coord:downstream-of=[<owner>/]<repo>#<n>`         | Same as upstream-of                                        |
| `coord:stacked-on=#<n>` or `=[<owner>/]<repo>#<n>` | Contains `#`; `<n>` parses as a Rust **`i32`** (same domain as upstream-of — `#2147483648` is rejected); empty repo part = same repo, a **non-empty** one gets the same both-segments check. **Code stacks only — never for migration ordering (coord derives that from `down_revision`).** |
| `coord:requires-tag=<pattern>`                     | Any non-empty value                                         |
| `coord:merge-strategy=squash\|rebase\|merge`       | One of the three exact strings                              |
| `coord:blocked`                                    | Flag (no value) — accepted, but **NOT a hold**              |
| `coord:experimental`                               | Flag — accepted, but **NOT a hold**                         |
| `coord:credibility-override`                       | Flag — Tier-7 credibility-gate escape hatch                 |
| `coord:migrate-repair`                             | Flag — accepted. **The one flag here that RELEASES a hold** rather than restricting: it can make a land happen that otherwise would not. coord bounds it at the *consuming* end, not the validator — `merge_scheduler::migrate_self_blocking` refuses to honour it unless the land is genuinely self-blocking, **and is further scoped to `EXPECTED_WEB_REPO` and the `PendingHead` escalation arm only**. Setting it is cheap and auditable; acting on it is not, and coord keeps those two decisions apart |
| `coord:priority` / `coord:priority=*`              | REJECTED — set it on the PR itself with `gh pr edit --add-label coord:priority`. A skill-set row writes `source='coord_skill'` and the merge scheduler only honours `source='github'`, so it would be inert (and invisible on GitHub). **Both spellings hit a bespoke error that names that fix** (coord's `PRIORITY_LABEL_ERR`); the parameterised form is caught deliberately, because the lever is ONE BIT and an author writing `=1` is reaching for numeric levels that do not exist |
| `coord:red-main-fix`                               | REJECTED — a flag with no `=`, so it lands on the generic `parameterised labels need "=value"`. coord has **no bespoke arm for it and this mirror must not invent one.** If you want it on the PR anyway, `gh pr edit --add-label coord:red-main-fix` — but understand it buys nothing: see below |
| `coord:operator-review`                            | REJECTED — retired label; labels no longer hold PRs (convert the PR to draft, or register a coord gate with a `MergePr` continuation) |
| `coord:version-bump` / `coord:version-bump=*`      | REJECTED — same retirement as operator-review (coord rejects the bare flag too) |
| `coord:state=*`, `coord:blocked-by=*`, `coord:specialist-decision=*` | REJECTED — coord-SET labels, read-only through this surface; change them at the state-mutation surface, not here |

A bare `<repo>#<n>` (no owner) passes validation and is canonicalized to
the tenant's `owner/repo#<n>` at coord's write surface via
`coord.tenant_repos`; coord rejects it there if the repo is unresolvable
or ambiguous — prefer writing the full `owner/repo#<n>` form **where it
fits**.

> **GitHub caps a label name at 50 characters**, and the full form overflows
> that for this fleet's longer repo names — so "prefer the full form" is not
> always achievable. `gh label create` fails
> `HTTP 422 … name is too long (maximum is 50 characters)`, and the subsequent
> `gh pr edit --add-label` then fails `'<label>' not found`, which reads like a
> missing-label problem rather than a length one. With the 8-character owner
> `qontinui` and a 4-digit PR number, the FULL `owner/repo#n` form overflows
> once the repo name reaches:
>
> | Prefix | Full form overflows at repo-name length |
> |---|---|
> | `coord:downstream-of=` (20 ch) | >= 17 characters |
> | `coord:upstream-of=` (18 ch) | >= 19 characters |
> | `coord:stacked-on=` (17 ch) | >= 20 characters |
>
> A longer owner, or a 5-digit PR number, shifts each threshold down by one per
> extra character. **This is deliberately a rule and not a list of repo names**:
> a list goes stale on every rename, and the one #297 shipped already had — it
> named three repos for `downstream-of`, but `qontinui-devtools` and
> `qontinui-finetune` are 17 characters and overflow too.
>
> **The short form fits any repo name up to 25 characters.** Dropping the owner
> leaves `20 + name + 5` for `downstream-of`: a 25-character name lands exactly
> on 50, and 26 goes one over. The longest name in the org today is 23, which is a snapshot — the
> 25-character bound is the part that stays true.
>
> **When it overflows, drop the owner** — `coord:downstream-of=<repo>#<n>` is
> the supported short form, canonicalized at coord's write surface via
> `coord.tenant_repos`. It is not a workaround; it is the grammar's own
> owner-optional arm. First hit 2026-08-19 wiring
> qontinui-claude-config#296 to qontinui-dev-notes#167 (51 chars, one over).
>
> **You do not have to count characters** — `set-label.sh` pre-flights the
> ceiling before it calls `gh` and, for a dep label carrying an owner, prints
> the owner-dropped label that would fit. Use `--dry-run` to check a label
> without sending anything — it closes **this** (length) cause, and says plainly
> that it cannot check the other one, whether the label exists.

**The ceiling is a skill-side guard, not part of the coord mirror.** coord's
`validate_label` has no length rule and should not grow one: `coord.pr_labels`
stores a text column and the 50-character cap belongs to the GitHub API. The
check therefore lives outside the mirrored function in `set-label.sh` — a sync
against `labels_routes.rs` must not delete it as "not in coord".

Rejected labels exit non-zero with a one-line error. Coord-set labels
(`coord:state=*` etc.) are explicitly rejected with a pointer to the
state-mutation surface.

### `coord:red-main-fix` — rejected here, and it buys nothing anywhere

This skill cannot set `coord:red-main-fix`, and the thing people reach for it
to do **does not exist**. Do not read the rejection as "use `gh` instead to get
the recovery lane" — there is no recovery lane to get.

- **The label is not an input to the merge predicate.** No merge-engine path
  reads it; `policies::evaluator::is_recovery_candidate` says so verbatim — "a
  mislabeled (or unlabeled) PR is judged purely on these facts". That is
  deliberate, so a mislabeled PR can never force-land.
- **The in-predicate waiver it names still cannot be relied on.** It requires
  `rebased_candidate_green`, whose only producer is
  `pr_merge::engine::head_has_green_speculative_candidate` (a green,
  non-invalidated `coord.speculative_chains` row). Speculative candidate CI is
  ARMED in production since the arm PR of plan
  `2026-07-25-coord-speculative-push-before-gate-churn` §8.4 step 6
  (2026-09-03, qontinui-coord#1894):
  `COORD_SPECULATIVE_DISABLED` is now an ordinary default-ON kill switch —
  `"1"` disables, unset arms, and `deploy/taskdef.json` sets `"0"` — so that
  producer CAN produce rows and the waiver is no longer inert BY THAT CAUSE.
  What remains is the bootstrap gap plan
  `2026-08-20-coord-red-main-recovery-lane-is-inert` records: a Tier-4-blocked
  PR never gets a proposal, so no chain is ever built for it. coord's
  `fixer_arm_readiness::adjacent_breakages` entry
  `red_main_recovery_merge_lane_inert` now derives its state from the live
  flag read rather than asserting a prod value. **Never wait for it to fire.**
- **Even if it fired it excludes security-class changes** (`!security_class_touched`),
  which is exactly the cargo-audit/RUSTSEC case people bring it to.

What actually lands a red-main fix is coord's **ordinary** merge path: `main-red`
is checked only at ENQUEUE (Tier 4 of `pr_merge::predicate::is_simple_green_path`)
and is never re-consulted at land. So open the fix PR green and non-draft and let
coord land it; never `gh pr merge --admin`. Applying the label as *intent
signalling* is still fine — `gh pr edit --add-label coord:red-main-fix` — just do
not expect it to change scheduling.

**coord itself will still tell you otherwise — that is a known, tracked defect,
not a signal the lane works.** `diagnose` emits levers reading *"Open a fix PR
and label it `coord:red-main-fix`"* (`diagnose.rs`, four live lever/`why`
strings plus a test asserting the text), and the autodispatch prompt in
`next_step.rs` makes the same promise to a machine. Verified still present on
coord `origin/main` @ `da36d08d` (2026-08-22). The repair is planned but NOT
shipped — plan `2026-08-20-coord-diagnose-emits-the-falsified-red-main-fix-lever`
(VETTED 2026-08-21), with the underlying inertness in
`2026-08-20-coord-red-main-recovery-lane-is-inert` (IN PROGRESS). **Until those
land, treat a `coord:red-main-fix` lever in `diagnose` output as falsified
guidance served from code, and do not act on it.**

Full derivation: `.claude/commands/merge-train-steward.md`; the consumer-facing
versions are in `.claude/commands/publish-runner.md` and
`prompts/coord-system-fixer-playbook.md`.

> Note the split of duties: the **validator** stays a faithful mirror of
> `labels_routes.rs` and therefore emits only coord's generic message, while the
> **doctrine lives here**. Adding a bespoke arm to `set-label.sh` for this label
> would be drift, and `set-label-selftest.sh` pins its absence.

## Outputs

On success, prints:

```
ok: gh added label "<label>" to <repo>#<pr>
ok: coord recorded label "<label>" in pr_labels (tenant_id=<uuid>, written=1)
```

On validation failure, prints the reason to stderr + exits non-zero:

```
error: stacked-on: missing "#<pr_number>"
```

Over-ceiling labels are rejected the same way, before `gh` is called, and the
error names the length so it is not mistaken for a missing label:

```
error: label is 51 characters; GitHub caps a label name at 50
```

With `--dry-run`, a label that passes both checks prints and exits 0 without
sending anything — followed by the one thing a dry run structurally cannot
establish:

```
ok: label "coord:downstream-of=qontinui-dev-notes#167" is valid (42/50 chars) -- dry run, nothing sent
note: NOT checked -- whether "coord:downstream-of=qontinui-dev-notes#167" exists as a label in qontinui/qontinui-claude-config.
      A dry run sends nothing, so it cannot ask. This is the one cause of
      "'<label>' not found" the ceiling check above does not cover.
      This key is open-valued, so its labels are not created on demand --
      a dep label's value is unique to the PR pair it wires. If nobody has
      created this one, a real send fails until you run:
        gh label create "coord:downstream-of=qontinui-dev-notes#167" --repo qontinui/qontinui-claude-config
```

The `gh label create` half appears only for an **open-valued key** —
`upstream-of`, `downstream-of`, `stacked-on`, `requires-tag`. It is deliberately
keyed rather than triggered by the presence of `=`: a flag label
(`coord:blocked` and friends) **and `coord:merge-strategy=`**, whose value is one
of exactly three strings, are both repo-wide labels somebody creates once, so
both get the caveat without a pointer to a repo-wide mutation nobody needs. A
`carries a value` test would sweep `merge-strategy` in with the dep labels and
then explain the advice with "unique to the PR pair", which is a claim about a
key it is not.

Neither form ever says the label *does not exist* — a dry run sent nothing and
has no evidence either way, and reporting an already-created label as absent
would be a fresh mis-signpost rather than a fix.

On a coord-side error, prints the coord response + exits non-zero. The
GitHub-side label add still succeeds first — if you need to remove
it, run `gh pr edit <pr> --remove-label "<label>"`.

## Failure modes

- **Missing `QONTINUI_AGENT_ID`** — skill exits with explanation; agent
  was spawned without the env-var (rare; report to operator).
- **HTTP 422 `tenant_resolution_failed`** — `QONTINUI_AGENT_ID` must be
  an agent id coord can resolve to a tenant via `coord.agent_worktrees`
  (an `~/.qontinui/agent-runs/<uuid>` id qualifies). A session id
  (`~/.qontinui/agent_session_id`) or a gate `registered_by` id does NOT
  resolve and is rejected. The skill exits non-zero with the body; the
  gh-side label (canonical) is already applied.
- **`'<label>' not found` from `gh pr edit --add-label` has TWO causes** —
  and only one of them is a missing label.
  1. **The label was never created.** Dynamic-value labels do not exist until
     someone makes them: run `gh label create "<label>" --repo <owner>/<name>`
     once, then re-run the skill. **You should not have to look that up here** —
     having already cleared the ceiling, `set-label.sh` knows a `'<label>' not
     found` at this point can only be case 1, so it relays gh's own line and
     then prints the exact `gh label create` command underneath it. It prints
     the command rather than running it: creating a label changes the repo's
     label set for every future PR, which is not what you asked this skill to
     do. The match requires gh to have NAMED the label, so an unresolvable repo
     or PR — which fails with the same two words — does not collect the advice.
     **`--dry-run` cannot reach this cause**: it sends nothing, so it cannot ask
     GitHub whether the label exists. It says so in its own report rather than
     letting `is valid` imply otherwise — see Outputs above.
  2. **The label is over 50 characters**, so it *cannot* exist —
     `gh label create` rejects it with
     `HTTP 422 ... name is too long (maximum is 50 characters)`, and creating
     it is not a fix. `set-label.sh` pre-flights this and rejects the label
     with an explicit length error before `gh` is reached, so a genuine
     overflow should no longer land you on case 1's advice — if you do see the
     raw gh error for an over-length label, the pre-flight was bypassed (label
     set by hand, or an older copy of the script). The two arms are
     complementary: the ceiling check catches case 2 *before* the call, and the
     diagnosis above catches case 1 *after* it, so neither cause reaches you as
     a bare `'<label>' not found`.
- **`gh` CLI unauthenticated** — `gh auth status` first; skill bubbles
  up the auth error from gh. It bubbles up everything else gh says too, on
  success as well as failure: `set-label.sh` captures gh's stderr in order to
  answer the one failure above, and re-emits it verbatim on every path, so
  routine notices (`A new release of gh is available`, deprecation and
  auth-scope warnings) are not eaten by the diagnosis.
- **Coord unreachable** — gh-side label add succeeds (GitHub is the
  canonical state), but `coord.pr_labels` will be out of sync until
  the reconciler watcher (Phase 1 D1.5) catches up on its next tick.

## Files

- `SKILL.md` — this file.
- `set-label.sh` — the bash wrapper. Validates, calls `gh api` (issues/labels),
  POSTs to coord.
- `set-label-selftest.sh` — runs the shipped validator over a known-bad /
  known-good corpus via `--dry-run`: no network, and no real `gh` — a stub
  shadows it on `PATH` as a tripwire, and the run asserts `--dry-run` never
  reached even that (the shadow itself is asserted first, so "no record" is not
  vacuous). The 50/51-character boundary cases are anchored by asserted length,
  so a repo rename fails the test loudly instead of quietly sliding the corpus
  off the edge it tests. It also pins what an ACCEPTED dry run says about
  itself: that the report names the existence check it could not run; that an
  open-valued key names the `gh label create` this would need while a flag label
  and the closed-enum `merge-strategy` do not (the case that pins the arm as
  KEYED rather than a `carries a value` test, since that looser form would sweep
  `merge-strategy` in); that neither asserts an absence a dry run has no evidence
  for; and that the original `is valid` line survives beside the caveat. One later
  section deliberately *does* reach a (second)
  stub gh, to pin the diagnosis above: that the label-not-found shape names
  `gh label create`; that neither an unrelated failure nor a bare `not found`
  which never named the label collects that advice; that gh's own stderr
  survives being answered on every path, success included; and that gh's stdout
  still lands on stdout. The success case captures the two streams separately,
  because a merged capture cannot tell the fd ordering apart at all — under the
  reversed spelling gh's stderr leaks to the real stdout instead of being
  captured, and the merged text is identical either way. Every case also asserts
  that the stub was actually invoked, so a regression that exits before `gh` is
  reached fails loudly instead of satisfying the negative assertions by never
  running.

## See Also

These references live in repos you may not have checked out
(`qontinui-dev-notes`, `qontinui-coord`); skip any whose repo is absent
under `<workspace-root>/`.

- `<workspace-root>/qontinui-dev-notes/docs/coord/pr-merge-labels.md` —
  full namespace + trailer equivalents + conflict resolution.
- `<workspace-root>/qontinui-coord/crates/coord/src/pr_merge/labels_routes.rs` —
  coord-side validator + ingest handler (single source of truth).
- `<workspace-root>/qontinui-dev-notes/plans/2026-05-21-pr-merge-orchestrator-design.md` —
  Phase 2 D2.6 spec.
