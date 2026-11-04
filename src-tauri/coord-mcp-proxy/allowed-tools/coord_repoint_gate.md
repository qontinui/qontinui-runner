`coord_repoint_gate` is IN for the gate-verb family's reason, one verb later
(qontinui-coord#2056, plan
`2026-09-09-pr-fix-autodispatch-arms-into-a-thin-brief-a-fifo-queue-and-a-gate-that-cannot-be-re-armed`
Phase 3a). It moves an OPEN gate's predicate to where a superseded PR went,
carrying the continuation, and withdraws the source — the ONLY way to re-arm a
superseded watch, because `continuation_spawn` is write-once at registration.
coord grants it on the device floor and its core enforces the REGISTRANT rule
(the same floor `coord_withdraw_gate`, already here, rests on). Withheld, the
remedy `coord_gate_doctor`'s `continuation_cancelled_not_rearmed` smell names
would answer `-32601` from inside the product — the supersede would keep losing
its arm silently, which is the defect the verb exists to end.

The registrant rule is no longer the WHOLE rule, so forwarding it does not reach
"only gates this device registered" (plan
`2026-09-23-an-adopter-cannot-re-anchor-a-gate-it-did-not-register`). When the
registrant check fails, coord runs a narrower SUPERSESSION arm: a non-registrant
device may re-point a `pr_merged` gate, including a system-registered one with
no recorded registrant. It may do so only when the source's continuation was
never cancelled, and when coord holds a live, tenant-confined
`supersession_declared` row naming the supplied predicate as a recorded
successor. That is a wider REACH under a coord-provable precondition, not a
wider TRUST. `coord_declare_supersession`, forwarded beside it, is the direct
door that writes that row when the successor's title token recorded nothing.
coord verifies every precondition itself (`record_agent_declaration`): it holds a
record of the predecessor, the caller's tenant owns both repos, the successor
LANDED by coord's own record and is not a fork, the successor's title or body
references the predecessor (the provenance link that stops a device pairing two
arbitrary PRs), its author is trusted or may write to the repo, and the
predecessor has not itself landed. Such a row feeds the gate move, the card's
`superseded_by` and the deferred-fail probe, NEVER a coord close: the
superseded-predecessor closer skips agent-declared rows and closes only on a
title declaration. Without that door, when the title recorded nothing, only the
gate's registrant could move it, and no operator twin of the re-point exists.
