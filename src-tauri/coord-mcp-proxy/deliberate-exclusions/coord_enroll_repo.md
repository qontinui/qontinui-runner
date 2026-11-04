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

Eight of them were added on the classification of finding `31a69d12` (record:
coord finding `4ba64e8b`, correcting `73b15a9b`). None of them was forwarded
before, so listing it here takes nothing away. Four join an existing family
in the DELIBERATELY EXCLUDED note (also in this file), and the fleet-drain writes start a new one (per-agent
grants):

* `coord_enroll_repo`: the onboarding / enrollment family the allowlist note
  excludes. The `coord_onboard` prefix does not catch its name, so it is
  listed explicitly. Its HTTP twin is on coord's device/agent router, so a
  session that created a repository can still enroll it over HTTP.
