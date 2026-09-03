---
name: coord-pr-label
description: Set or retract coord:* labels on a pull request through coord's ONE label door — declare intent (upstream-of/downstream-of/stacked-on dependency edges, requires-tag, merge-strategy, blocked/experimental flags) so the PR Merge Orchestrator schedules the auto-merge correctly. From a session with coord-mcp, call the MCP tools coord_pr_label_set / coord_pr_label_unset directly; set-label.sh is the shell/CI fallback. coord writes GitHub FIRST and then its own row, so a label can never be on one side and not the other. All three dep labels work cross-repo with the [<owner>/]<repo>#<n> grammar; no label holds a PR; --dry-run asks coord to validate without writing. Tenant resolved from your session's JWT — no agent id, no URL to export.
user-invocable: true
---

# coord-pr-label

Declare `coord:*` labels on a pull request to express PR-merge-orchestrator
intent — through **one door**, which owns both halves of the declaration.

## The door, and why there is only one

`POST /coord/pr-labels` (declare) and `DELETE /coord/pr-labels` (retract) on
coord, device/agent-JWT authed. For every label coord:

1. validates it against the `coord:*` namespace and canonicalizes a bare
   `<repo>#<n>` to `owner/repo#<n>` against **your tenant's** repos;
2. writes it to **GitHub first** (App client, bounded retry — the REST issues
   route, which also creates a missing dynamic-value label);
3. only then records the `coord.pr_labels` row, as `source='github'`, so the
   ordinary webhook / hydration reconciles own it — removing the label on
   GitHub retracts it;
4. re-syncs the dependency edges **before answering** — the edge exists when
   the call returns, so the merge tick cannot race a webhook.

A GitHub failure yields a `rejected[]` entry and **no row**. A dependency edge
that would close a cycle is **undone on GitHub** and leaves no row. So the
failure the old two-step skill produced seven times — label visible on the PR,
no edge in coord, PR scheduled as independent — cannot be produced by this
door at all. The MCP tools and the shell script call the same server code.

(History: until 2026-09-03 this skill ran `gh` and then POSTed to an anonymous
`/pr-merge/labels` whose default URL was a port nothing served, with a
client-side validator that drifted from coord five times. Dossier
`coord-pr-label-half-write`; plan
`2026-08-27-coord-pr-label-write-path-single-door`.)

## How To Use

### From a Claude Code session (the sanctioned path)

Call the MCP tools directly — no credential, URL or agent id to arrange:

- **Declare:** `coord_pr_label_set(repo="qontinui/qontinui-coord", pr_number=75,
  labels=["coord:upstream-of=qontinui/qontinui-schemas#42"])`
- **Set semantics:** add `mode="replace"` — the posted set becomes the PR's
  complete author-settable `coord:*` declaration; everything else
  author-settable is retracted from GitHub and coord. `labels=[]` with
  `mode="replace"` is a total retraction.
- **Check without writing:** add `dry_run=true`.
- **Retract one label:** `coord_pr_label_unset(repo=..., pr_number=..., label=...)`.

Read `rejected[]` in the answer per label — it is data, not an error. `ok` is
false only when nothing was declared.

### From a shell or CI (the fallback)

`set-label.sh` sits next to this SKILL.md; spell its path relative to THIS
SKILL DIR — `<path-to-this-skill-dir>/set-label.sh` — never through a
`qontinui-claude-config` checkout (the skill is provisioned into
`<session-workdir>/.claude/skills/coord-pr-label/` on devices with no such
checkout).

```bash
bash <path-to-this-skill-dir>/set-label.sh \
  --repo qontinui/qontinui-coord --pr 75 \
  --label "coord:upstream-of=qontinui/qontinui-schemas#42"
```

```bash
# stack on a cross-repo parent, replacing whatever was declared before
bash <path-to-this-skill-dir>/set-label.sh \
  --repo qontinui/qontinui-coord --pr 75 \
  --label "coord:stacked-on=qontinui/qontinui-web#748" --replace
```

```bash
# validate only — coord answers, nothing is written
bash <path-to-this-skill-dir>/set-label.sh \
  --repo qontinui/qontinui-coord --pr 75 \
  --label "coord:downstream-of=qontinui-dev-notes#1234" --dry-run
```

```bash
# retract one label from both stores
bash <path-to-this-skill-dir>/set-label.sh \
  --repo qontinui/qontinui-coord --pr 75 --unset "coord:blocked"
```

The script reaches the door through a cascade and reports which rung answered:

1. the local runner's coord-mcp write forwarder (`<proxy-url>/pr-labels`, nonce
   from a runner-written `.mcp.json` near `$PWD`; the runner injects a fresh
   device JWT — nothing to export);
2. coord directly at `${COORD_HTTP_URL:-https://coord.qontinui.io}/coord/pr-labels`
   with `$COORD_AGENT_JWT`, else `$COORD_DEVICE_JWT`, else
   `~/.qontinui/coord-device-jwt`.

Exit codes: `0` everything declared/retracted; `1` coord refused some or all
labels (one `rejected:` line each, nothing partial left behind for them); `2`
usage; `4` no door answered — and then **nothing was written anywhere**, which
the message says. `--json` prints the raw answer after the summary lines.

## When To Use

- **Declaring a dependency**: all three dep labels share one value grammar —
  `[<owner>/]<repo>#<n>` (`stacked-on` also keeps bare `#<n>` for same-repo).
  Only the must-land-second side of an edge waits:
  - "this PR must merge after qontinui/qontinui-schemas#42" →
    `coord:downstream-of=qontinui/qontinui-schemas#42` (this PR waits;
    #42 unaffected).
  - "that PR must merge after this one" →
    `coord:upstream-of=qontinui/qontinui-web#99` (the carrier does NOT
    wait; #99 waits until this PR lands).
  - **Stacking a PR on a parent**: `coord:stacked-on=#42` (same repo) or
    `coord:stacked-on=qontinui/qontinui-web#42` (cross-repo) — this PR
    waits until the parent lands. **Code stacks only — see the migration
    callout below.**

  Labels on the waiting side are auto-stripped when the parent lands; two
  green PRs with a declared edge land in dependency order, unattended. If a
  waiting-side label ever survives its parent's land, read the reconciler
  metrics in the order `knowledge-base/qontinui-specific/coord-merge-train.md`
  gives — never `repo_branches.close_cause`, which is sticky.
- **Pinning a required tag**: `coord:requires-tag=ts-v*`.
- **Withdrawing a dependency**: `coord_pr_label_unset` (or `--unset`) — the
  edge is gone from coord and the label from GitHub when it returns. This is
  the retraction path `qontinui-runner#1153` / `#1147` lacked.

> **No label holds a PR** (holds retired 2026-06-20). To hold a PR: **convert
> it to draft**, or **register a coord gate with a `MergePr` continuation**.
> `coord:blocked` / `coord:experimental` are accepted — they route
> dequeue-time merge-class as flag labels — but they do NOT block merge.
> `coord:operator-review` and `coord:version-bump=*` are retired and REJECTED.

> **Do NOT use `coord:stacked-on` / `coord:upstream-of` /
> `coord:downstream-of` to order a *migration* stack.** coord derives that
> ordering from the `down_revision` chain
> (`qontinui-coord/crates/coord/src/pr_merge/dep_graph.rs`
> `predict_migration_stacks`) with no label required. Reserve the dep labels
> for genuine **code** stacks.

**Don't use** to set `coord:state=*`, `coord:blocked-by=*`,
`coord:specialist-decision=*` (coord-set, read-only through this door) or
`coord:priority` (set it on the PR itself with
`gh pr edit --add-label coord:priority`; the priority lane honours
`source='github'` rows written by the webhook only, and the door tells you so).

## Validation

Validation is coord's, in `labels_routes.rs::validate_label` — there is no
client-side mirror any more, so nothing here can drift from it. Accepted:

| Label | Rule |
|---|---|
| `coord:upstream-of=[<owner>/]<repo>#<n>` | contains `#`; repo part non-empty, both segments non-empty when it carries a `/`; `<n>` parses as a Rust `i32` |
| `coord:downstream-of=[<owner>/]<repo>#<n>` | same as upstream-of |
| `coord:stacked-on=#<n>` or `=[<owner>/]<repo>#<n>` | `<n>` parses as `i32`; empty repo part = same repo |
| `coord:requires-tag=<pattern>` | any non-empty value |
| `coord:merge-strategy=squash\|rebase\|merge` | one of the three |
| `coord:blocked`, `coord:experimental`, `coord:credibility-override`, `coord:migrate-repair` | flags — accepted; **not holds** (`migrate-repair` is bounded at the consuming end) |
| `coord:priority[=*]`, `coord:operator-review`, `coord:version-bump[=*]`, `coord:state=*`, `coord:blocked-by=*`, `coord:specialist-decision=*`, `coord:red-main-fix` | REJECTED, each with coord's own reason in `rejected[]` |

A bare `<repo>#<n>` is canonicalized to `owner/repo#<n>` against your tenant's
registered repos; an unresolvable or ambiguous bare repo, or an owner-qualified
repo not registered to your tenant, is rejected with the reason. Prefer the
owner-qualified form where it fits GitHub's 50-character label-name ceiling;
when it does not, drop the owner — the short form is the grammar's own
owner-optional arm, not a workaround.

`coord:red-main-fix` buys nothing anywhere: the label is not an input to the
merge predicate, and the in-predicate waiver it names still cannot be relied on.
Speculative candidate CI is ARMED in production since the arm PR of plan
`2026-07-25-coord-speculative-push-before-gate-churn` §8.4 step 6
(qontinui-coord#1894, 2026-09-03 — `COORD_SPECULATIVE_DISABLED` is now a real
default-ON kill switch), so the waiver's `rebased_candidate_green` producer can
produce rows; what remains is the bootstrap gap plan
`2026-08-20-coord-red-main-recovery-lane-is-inert` records (a Tier-4-blocked PR
never gets a proposal). What lands a red-main fix is coord's ordinary merge
path. Full derivation: `.claude/commands/merge-train-steward.md`.

## Outputs

Declare (script), on success:

```
ok: declared "coord:upstream-of=qontinui/qontinui-schemas#42" on qontinui/qontinui-coord#75 — on GitHub and in coord (source=github), edges synced
```

Refusal (exit 1):

```
rejected: "coord:stacked-on=x" — stacked-on: missing `#<pr_number>`
note: 1 label(s) refused by coord; nothing partial was left behind for them.
```

No door (exit 4):

```
error: no coord door answered — NOTHING was written, on GitHub or in coord.
```

The MCP tools return the same fields as JSON: `valid`, `written`, `deleted`,
`rejected[] {label, reason, cycle?}`, `github {added, removed}`, `ok`.

## Files

- `SKILL.md` — this file.
- `set-label.sh` — the thin shell client: builds the request, walks the
  transport cascade, renders coord's answer. No `gh`, no local validator.
- `set-label-selftest.sh` — hermetic: a PATH-shadowed `curl` stub records every
  request and answers from fixtures; a `gh` stub fails loudly if ever called.
  Pins the request shape (POST/DELETE, `mode`, `dry_run`, labels verbatim), the
  cascade (nonce rung → bearer rung on 401 and on an old runner's 404; coord's
  typed 404 is an answer), the verdicts (exit 0/1/2/4) and that the script
  carries no `:9870` default and no `gh` call.

## See Also

- `<workspace-root>/qontinui-coord/crates/coord/src/pr_merge/labels_routes.rs` —
  the door (single source of truth for validation and for what is written).
- `<workspace-root>/qontinui-dev-notes/docs/coord/pr-merge-labels.md` — full
  namespace + trailer equivalents + conflict resolution (skip if the repo is
  not checked out).
- `<workspace-root>/qontinui-dev-notes/plans/2026-08-27-coord-pr-label-write-path-single-door.md`
  — why one door, and the seven occurrences that made it necessary.
