---
name: pr-status
description: Show fresh PR status cards from coord's twin — never from memory. Calls the coord_pr_status MCP read (mine=true for your own authored PRs, or a specific repo+number) and renders each card with an explicit freshness indicator. Use this instead of grepping memory + `gh pr view` when you need to know what a PR is doing right now; PR state is high-churn twin state, so a memory snapshot is stale the moment it's written.
user-invocable: true
---

# pr-status

Answer "what is PR #N doing right now?" from coord's **freshness-gated twin**,
not from a memory file. PR status is continuous, high-churn state; a memory line
is a snapshot written at one instant and is stale the moment it lands (see the
`#676` incident: memory said "CONFLICTING against main" when the PR had **merged
4.5h earlier**). coord already models per-PR state with an honest freshness
stamp — this skill is the session-facing read of it.

## What it does

Calls the **`coord_pr_status`** MCP read and renders the returned card(s). The
tool is read-only, fleet-wide, and needs **no device-scoped JWT** — it works on
the standard session identity (it mirrors `coord_query_ci_state`'s auth-agnostic
handler).

Two shapes:

- **`/pr-status`** (no args) → `coord_pr_status(mine=true)` — the PRs authored by
  *this* session (resolved via the PR↔session worktree mapping). Renders one card
  per authored PR.
- **`/pr-status <owner/repo> <number>`** → `coord_pr_status(repo, number)` — one
  specific PR's card, including **post-land** PRs (a merged PR still answers, with
  `merged_at` + `merge_commit`; if the row aged out after branch deletion the card
  is reconstructed from `pr_events` and flagged `reconstructed_from_events`).

## Instructions

1. **Resolve the call shape** from the args:
   - No args → call the MCP tool `coord_pr_status` with `{ "mine": true }`.
   - `<owner/repo> <number>` → call `coord_pr_status` with
     `{ "repo": "<owner/repo>", "number": <n> }`.
2. **Call the tool.** `coord_pr_status` is in the read-only tool allow-set, so a
   normal session can call it directly. If it comes back
   unknown/method-not-found (per-agent allow-set masking), fall back to the HTTP
   route via `pr-status.sh`, which sits next to this SKILL.md — run it as
   `bash <path-to-this-skill-dir>/pr-status.sh --mine` (or
   `--repo <owner/repo> --number <n>`), never through a `qontinui-claude-config`
   checkout, which a provisioned copy of this skill does not have.
   It mirrors the `/gate` transport
   cascade: first a sweep of the runner-written `.mcp.json` proxy doors
   (loopback only), then the acting-bearer token. **Never** claim a status you
   did not read from the twin. If it fails, the error names WHY (a local fault
   is reported as one, never as "coord unreachable"); if the transport is fine
   and coord declined the call, it says so and stops rather than retrying.
   The acting-bearer half reports the same typed causes `/coord-revive` does —
   `HELPER_NOT_FOUND` / `NO_TOKEN` / `MINT_FAILED` / `HELPER_DEPS_MISSING` — so
   "the helper is missing" and "coord refused the mint" are never both reported
   as `$COORD_AGENT_JWT` being unset. A sweep that found **no** door says so
   too, rather than letting the bearer note stand for the whole cascade.
3. **Render each card** as a compact, honest line. Lead with the freshness so the
   reader can never mistake a stale read for a fresh one:

   ```
   qontinui-runner#676  MERGED  ✔ fresh (verified 13:54Z)
     merge_commit 7b5814ec · merged_at 2026-07-06T13:54Z
   ```
   ```
   qontinui-coord#988  OPEN  ⚠ STALE (last verified 41m ago — refresh before acting)
     merge_state: dirty · mergeable: false · checks: 4/5 · blockers: textual conflict
     deps: stacked-on qontinui-schemas#42
   ```

   Freshness glyphs from the card's `confidence` field:
   - `fresh` → `✔ fresh`
   - `stale` → `⚠ STALE (refresh before acting)`
   - `unknown` → `? UNKNOWN (never hydrated — refresh before acting)`

   Always print `last_verified_at`. Print `blockers`, `required_checks` rollup,
   and `dep_edges` when non-empty. For a reconstructed card, print
   `(reconstructed from pr_events — repo_branches row aged out)`.
4. **Never cache the result in memory.** The whole point is that this read is
   always fresh and self-describing. If you want to record something durable
   about the PR, record the **narrative** (why it exists, its recurring failure
   pattern, what supersedes it) keyed by PR *number* — never its live status.
   See memory `feedback_pr_status_is_twin_state`.

## Why not `gh pr view` + memory?

- `gh pr view` is fresh but un-cached and un-contextualized — you re-derive it
  every time (the `#676` session ran it ~8 times and still held a wrong model).
- A memory line is contextualized but silently stale.
- `coord_pr_status` is both fresh **and** self-describing about its freshness
  (`confidence` + `last_verified_at`), and it retains post-land state — the one
  thing a bare `gh` call and a memory file each individually cannot give you.
