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

* `coord_cancel_merge`: coord also put it on the device floor (2026-08-09,
  plan `2026-07-30-coord-tenant-self-service-merge-recovery`, coord#1409).
  Its HTTP twin `POST /pr-merge/tenant/proposals/:id/cancel` admits a device,
  so a session that must withdraw its own tenant's proposal still has that
  door. It stays withheld HERE for the same reason as `coord_create_pr`: the
  non-goal of plan
  `2026-08-22-primary-runner-build-drift-reads-as-a-policy-boundary` names it
  among the exclusions that are "decisions, not drift". It was explicitly
  left unmoved when `coord_reevaluate` was granted (2026-08-31). Reversing it
  is a decision to make on its own, not as a side effect of a parity sweep.

Two more are recorded here because coord grants them on the device floor and
this list masks them on purpose. They were found while classifying the
eleven names of finding `31a69d12`:
