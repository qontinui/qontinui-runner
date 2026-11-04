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

The direct supersession-declaration door (qontinui-coord, operator
directive 2026-10-03, served policy `git-operations`
`a-landed-adoption-closes-its-predecessor`). It records the declaration
`coord_repoint_gate`'s supersession arm needs when a successor's title
token was missing or not recorded — every precondition (coord knows the
predecessor, tenant owns both repos, successor LANDED and not a fork,
successor's text references the predecessor, author trusted or a repo
writer, predecessor not landed) is verified by coord. Withheld here it
would answer `-32601`, and only the gate's registrant could move it. It
never authorizes a coord CLOSE of the predecessor: the
closer skips agent-declared rows and acts only on a title declaration.
