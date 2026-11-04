# The gate-verb family is IN — plan `2026-08-10-agent-gate-management-must-ship-in-the-product`, P4

`coord_reject_gate`, `coord_reopen_gate`, `coord_mute_gate`, `coord_unmute_gate`,
`coord_snooze_gate`, `coord_cancel_continuation`, `coord_force_clear_gate` and
`coord_set_gate_audience` were drift of the worst kind: the one this door's own
warning is built to catch, sitting unread for days.

coord unmasked all eight on the device floor — six in `f9c9eef3` (P4a) and the
`force-clear` / `audience` pair on the 2026-08-20 operator decision — and P4
(`df8c2868`) then shipped the `gate_action` dial to govern them. This list never
moved, so from INSIDE the product every one of them answered `-32601`. That is
precisely the plan's own Null A restated one layer out: an interactive runner
session authenticates as the device principal and reaches coord through this
proxy, so a capability this list omits does not exist for the user, whatever
coord grants. The plan's §9 says so in advance — *"`device_default_allow_set()`
returning a name proves masking, not reachability"* — and this drift is what
running that acceptance for the first time actually found.

**The dial is the control surface, not this list.** Withholding `force-clear` or
`audience` here would not add a safety check; it would make `gate_action` unable
to do its job, because a dial can only govern what the door forwards. The real
checks all live downstream and are unaffected by the grant: clearance authority,
`agent_non_author`, the `gate_action` tiers, and a notification on every
sensitive action. Every one of these verbs is also reversible by `reopen`, which
is the invariant the plan requires of anything it grants.

`approve` stays unreachable — and it is unreachable by construction rather than
by this list, because coord defines no `coord_approve_gate` tool at all. The 10s
evaluation sweep already clears any gate whose predicate verifies, so the action
has nothing to do.
