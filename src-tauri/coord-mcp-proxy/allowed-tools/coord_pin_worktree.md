Finding `31a69d12` (posted by this runner itself on build `44691aaec4d4`)
reported eleven names that coord ADVERTISED to this door's principal, which
this list neither allowed nor named as deliberate. Each was classified; the
design-fork record is coord finding `4ba64e8b` (correcting `73b15a9b`, which
revised `e652d447`).
None of the eleven is on coord's device floor at coord `origin/main`
`1e720228b802`, so which coord build and principal advertised them is
unestablished. That is why forwarding
is limited to names whose call is harmless if coord refuses it, and safe if
coord grants it. Three are IN:

* `coord_check_repo_name`: an availability probe.
* `coord_create_repo`: creates an EMPTY repository under an owner bound to
  the caller's tenant. It publishes no code (its sibling
  `coord_push_credential` is excluded), but a repository it creates can be
  PUBLIC (`private: false`), which is visible outside the tenant. That side
  effect is accepted: the call is owner-bound and rate-limited per caller,
  and the repository is empty.
* `coord_pin_worktree`: sets a worktree's retention. Pinning keeps reclaim
  off it. Unpinning (`retention: auto`) applies to any `agent_id` in the
  tenant and hands the worktree back to reclaim. Reclaim's own dirtiness
  guard still refuses to remove a tree with uncommitted work, so nothing is
  deleted by this call itself.

The other eight are in [`COORD_MCP_DELIBERATE_EXCLUSIONS`], each with its
reason in that list's note.
