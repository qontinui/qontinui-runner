# Plan Graph

Render the dependency DAG across all plans (`$QONTINUI_PLANS_DIR`, plus
`$QONTINUI_PLANS_ARCHIVE_DIR` when set — see
[Environment overrides](#environment-overrides)).

Phase 3.3 of `2026-05-21-coordination-improvements`. Builds on Phase 3.1
(the `Depends-On:` frontmatter field) + Phase 3.2 (the
`resolve-plan-deps.py` single-plan resolver). This skill is the
**cross-plan view** — useful before starting new work to see what would
block, after a `/vet-plan` pass that discovers a dep, or when the
operator asks "what's blocking what?"

## Arguments

- `<stem>` (optional) — restrict the render to the subgraph reachable
  from this plan stem (= its transitive deps). Default: full graph
  across every directory the helper resolves (see
  [Environment overrides](#environment-overrides)).
- `--format text|json|mermaid` (optional, default `text`)
- `--include-shipped` (optional, default off — SHIPPED + SUPERSEDED +
  OBSOLETE leaves are hidden so the render focuses on what's still in
  motion)

`$ARGUMENTS` is passed through verbatim to the Python helper, so any
flag the helper accepts works here.

## Instructions

Shell out to the canonical helper:

```bash
python <workspace-root>/qontinui-stack/scripts/plan-graph.py $ARGUMENTS
```

Display the helper's output to the operator.

- For `--format text` (default), the output is already formatted as an
  indented tree with `[STATUS]` annotations on each node. Just paste it.
- For `--format json`, summarize node/edge counts plus any
  `missing[]` or `cycles[]` warnings, then offer to re-render as
  `text` on request.
- For `--format mermaid`, paste the output and remind the operator they
  can drop it into <https://mermaid.live> for a visual graph.

If the helper exits non-zero, surface the stderr and stop — don't try to
work around it inline.

## Behavior notes

- **Edge direction:** `A -> B` means "A depends on B" (B must ship
  before A). Roots of the rendered tree are plans nothing else depends
  on yet.
- **Missing deps:** if any plan declares `Depends-On: foo` but no
  `foo.md` exists in any resolved directory, the helper flags `foo` as
  `[MISSING]` and lists it in the JSON `missing[]` array. Treat this
  the same as `/vet-plan` Step 4 would — likely a typo or a renamed
  upstream plan; suggest the operator either fix the dep stem or remove
  the entry.
- **Cycles:** if a cycle is detected (e.g. `a -> b -> a`), the helper
  renders what it can and flags the cycle in the output. Don't try to
  fix it autonomously — surface it and let the operator decide which
  edge is wrong.
- **Status discipline:** a plan with no status blockquote is treated as
  `DRAFT`. A blockquote with no recognizable lifecycle word renders as
  `[?]`. These match the convention from `resolve-plan-deps.py` so the
  two skills stay consistent.

## When to use

- **Pre-flight before starting a new plan** — "what would block me?"
  Run with `--root <new-plan-stem>` to see only that plan's chain.
- **After a `/vet-plan` pass discovers a dep** — to see the broader
  picture and confirm no cycles or stranded prerequisites.
- **When the operator asks "what's blocking what?"** — run with no
  args for the full graph, or `--include-shipped` to include already-shipped
  predecessors as context.
- **Session start in `/next-steps`-style discovery** — quick overview
  of in-flight plans + their inter-dependencies.

## Examples

```bash
# Full graph, hide SHIPPED leaves (default behavior)
python <workspace-root>/qontinui-stack/scripts/plan-graph.py

# Subgraph from a specific plan
python <workspace-root>/qontinui-stack/scripts/plan-graph.py \
    --root 2026-05-21-coordination-improvements

# JSON for tooling
python <workspace-root>/qontinui-stack/scripts/plan-graph.py --format json

# Mermaid for mermaid.live
python <workspace-root>/qontinui-stack/scripts/plan-graph.py --format mermaid

# Include shipped predecessors for full historical context
python <workspace-root>/qontinui-stack/scripts/plan-graph.py --include-shipped
```

## Environment overrides

`plan-graph.py` honors the same two env vars as `resolve-plan-deps.py`, and the
qontinui runner injects both into agent sessions from its `paths.plans_dir` /
`paths.plans_archive_dir` settings. A session launched outside the runner will
not have them, in which case the helper uses the fallbacks below.

- **`QONTINUI_PLANS_DIR`** — the active plans directory. If unset, the helper
  falls back to `<workspace-root>/plans` (a `plans/` directory beside the repos
  this session is working in); if that does not exist either, it exits with a
  clear error rather than guessing a machine-specific path.
- **`QONTINUI_PLANS_ARCHIVE_DIR`** — optional archive directory: a real
  destination for archived plans, honored only when set. Unset means a
  single-directory layout — everything lives in the active plans dir and no
  second directory is searched. Archiving is a file location, not a lifecycle
  state: a plan moved into the archive keeps whatever status stamp it already
  carried.

Both are also settable per-invocation via `--plans-dir` / `--archive-dir`,
which take precedence over the environment.

Setting these explicitly is also what makes CI runs against a checked-out plan
tree at a non-default path reproducible.
