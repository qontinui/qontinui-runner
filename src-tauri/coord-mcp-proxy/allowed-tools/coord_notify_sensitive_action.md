`coord_notify_sensitive_action` is IN because withholding it would withhold a
REPORT, never an action. It is the notify half of NOTIFY-AFTER-ACTION
(`escalation-bar` `do-reversible-mechanical-work`, operator-revised
2026-08-30): a session that publishes a version or force-pushes a ref does so
through its own shell, not through coord, so filtering the tool cannot stop
the step — it can only stop the operator from hearing about it.
Authority-wise it is the twin of `coord_post_finding`, already granted here:
an append-only, self-attributed row about the caller's own action, whose
`kind` is hard-coded coord-side and whose `actor` is derived from the
credential, so it cannot forge a coord-internal notification.

Until plan `2026-09-18-notifications-are-agent-actions-and-alerts-are-agent-work`
Phase 4 this line named `coord_post_notification` — a name never served on main.
Coord's tool was `coord_notify_irreversible_action`, so the entry allowed
nothing and the real tool answered `-32601` from every runner-proxied session:
the self-report door was unreachable while this list read as if it were open.
Coord renames its tool to `coord_notify_sensitive_action` in the same phase,
and this entry carries the NEW name only — the old name is on no path a
runner session should be steered toward.
