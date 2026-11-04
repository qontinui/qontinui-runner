DELIBERATELY EXCLUDED (add only with a security rationale): the onboarding
/ enrollment family (`coord_onboard_*`, `coord_onboarding_doctor`,
`coord_enroll_repo`), privilege escalation (`coord_attest_escalate_override`,
`coord_attest_advisory_ci_override`), merge authority (`coord_request_merge`,
`coord_cancel_merge`, `coord_pr_merge_verdict`, `coord_pr_merge_profile`,
`coord_set_pr_draft`), code publication (`coord_create_pr`,
`coord_push_to_branch`, `coord_push_credential` — sessions open PRs via the
dedicated `/vcs/pull-requests` loopback route, not this proxy), reservations
(`coord_migration_reserve`, `coord_reserve_resource`), policy/state
mutation (`coord_request_policy`, `coord_flag_state`, `coord_flag_states`),
and per-agent levers coord would also hand to every un-curated agent
(`coord_fleet_drain`, `coord_fleet_undrain`, `coord_fleet_drain_host`,
`coord_fleet_undrain_host`).

Still OUT and unmoved: `coord_cancel_merge`, `coord_request_merge`,
`coord_create_pr`, `coord_push_to_branch` and the rest of
[`COORD_MCP_DELIBERATE_EXCLUSIONS`]. They are not the same shape — two are code
publication and two sit against the standing rule that coord is the sole merge
authority — so each is owed its own argument, not a sweep.
