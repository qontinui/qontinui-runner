`coord_session_worktrees` and `coord_reevaluate_dry` are IN, found by finally
doing the comparison mechanically instead of one `-32601` at a time. Diffing
coord's `mcp::agent_tool_access::DEVICE_DEFAULT_TOOLS` (45 names, coord
`origin/main` 16850b9) against this list turned up FIVE names coord grants a
device and this door refuses. Two are the merge-authority and code-publication
exclusions in `../deliberate-exclusions/`, deliberate and documented. The other three — these two plus
`coord_reevaluate` — appear nowhere in that rationale, so they were drift.

The evidence is that SOURCE diff, deliberately, and not a live `-32601`. A
denial from this door reports the allowlist compiled into the RUNNING binary,
which on the primary was 83 commits behind `origin/main` when this was written
— stale enough that `coord_agent_registry_effective` also answered `-32601`
despite having been allowlisted here since `9679a4649`. A refusal observed
live is a rebuild question until `/health` `buildDrift` says otherwise; it can
corroborate this list's contents but can never establish them.

Both earn the grant on their own terms. `coord_session_worktrees` is a
read-only join answering "what did my finished sessions leave behind?" — it
reaps nothing and stamps nothing, and it is how a session satisfies the
worktree-cleanup half of its own closeout. `coord_reevaluate_dry` is the
no-side-effect READ twin of `coord_reevaluate`; coord's own note on granting it
is that withholding a free confirmation step only pushes a caller toward the
destructive rung below it, which is precisely the effect this list was having.
