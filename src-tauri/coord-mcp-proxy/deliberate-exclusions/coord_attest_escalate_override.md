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

`coord_submit_escalate_evidence` is IN, and it is the reason this paragraph
exists: it is the agent door of the operator ruling
`decision_record/escalate-path-clearance-is-agent-work` — an
`escalate-path-matched` block is cleared by an agent submitting a grounded
evidence bundle for one head, and coord (not the agent) checks it, announces
the clearance, and writes the head-pinned marker. The grant carries no scope
and no glob; coord's refusals (`head_moved`, `disclosure_not_grounded`,
`review_not_for_head`, `credential_not_holder_issued`, …) are the control.
Withheld here it answered `METHOD_NOT_ALLOWED` from every runner-proxied
session, so the blocker line coord prints on every such PR — which names this
tool as the remedy — sent the session to a door this list shut (measured
2026-09-26 on claude-config#1198 and 2026-09-29 on coord#2638, where the
bundle had to be carried over a hand-minted device JWT instead). Its operator
twin `coord_attest_escalate_override` stays in
[`COORD_MCP_DELIBERATE_EXCLUSIONS`], unmoved.

* `coord_attest_advisory_ci_override`: one of coord's four
  `require_strategy_admin` tools. No device or agent token carries
  `strategy_admin` today. That is a mint convention, not a structural
  guarantee, so this exclusion keeps the override in the operator family. Its
  twin `coord_attest_escalate_override` is excluded as privilege escalation
  (and is also `require_strategy_admin`).
