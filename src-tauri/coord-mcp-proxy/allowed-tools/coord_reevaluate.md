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

`coord_reevaluate` — the WRITE twin — is now IN. It was held OUT with the note
that it "sits inside the merge-authority family already excluded above; the dry
read is the part a session needs to decide, and the act belongs to the train",
closing with the admission that this was "a judgement, not a mechanical
consequence, and the one entry here worth an operator's second opinion".

**The operator gave that second opinion on 2026-08-31**, as the governing
principle for plan
`2026-08-31-agent-reachable-reevaluate-and-honest-mint-failure`: *all coord
actions should be available to agents; sensitive actions should require posting
a notification, not an operator gate.* The prior paragraph is preserved above
rather than deleted, because the argument it makes is still the reason this is
the LAST entry of the merge-authority family to move — not evidence that it
should not have.

The grant carries no new authority. `POST
/pr-merge/prs/{owner}/{name}/{pr_number}/reevaluate`
(`pr_merge::tenant_routes::post_reevaluate`) is on coord's Tier-2 sub-router
under `require_jwt_or_operator`, whose `Tier2Caller::Coord` arm admits exactly
the device JWT a runner-hosted session already carries, and both doors call
`tenant_remediation::reevaluate_pr_for_tenant` with `RemediationActor::Tenant`.
coord put the tool on `DEVICE_DEFAULT_TOOLS` in `ac9388e9` (2026-08-09) for that
reason. Measured from inside a runner-hosted session on 2026-08-31,
`coord_can(call_tool, coord_reevaluate)` answered `allowed: true` while THIS
list still refused the call — the same shape as the four grants before it (`coord_write_prompt_document`,
`coord_agent_registry_effective`, `coord_session_worktrees`, `coord_reevaluate_dry`).

Nor is the act unguarded, and none of the guards live here: the tenant ownership
floor renders a cross-tenant target as 404, `RemediationActor::Tenant` applies
the per-tenant rate limit and keeps the terminal reap-hardcap unblock at 409, and
the merge predicate still decides merge-vs-hold. Re-running a gate cannot weaken
it. What withholding bought was not a boundary but a bare `-32601` on the one
lever `coord_diagnose` and five `tenant_levers_for_wedge` ladders publish by name
to precisely this principal.

The NOTIFY half of the operator's principle stays where policy already puts it:
`escalation-bar` `do-reversible-mechanical-work` obliges the calling session to
post it, and `coord_notify_sensitive_action` / `coord_post_finding` are both already on
this list. Enforcing the notification AT THIS DOOR is a separate design decision
(would a failed notification block the write?), surfaced as a held fork in the
plan rather than decided here.

Still OUT and unmoved: `coord_cancel_merge`, `coord_request_merge`,
`coord_create_pr`, `coord_push_to_branch` and the rest of
[`COORD_MCP_DELIBERATE_EXCLUSIONS`]. They are not the same shape — two are code
publication and two sit against the standing rule that coord is the sole merge
authority — so each is owed its own argument, not a sweep.
