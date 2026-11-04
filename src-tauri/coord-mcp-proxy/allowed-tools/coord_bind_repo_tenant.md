**The per-repo tenant binding writes are IN, with their risk stated** (plan
`2026-10-17-per-repo-tenant-binding-overrides-the-org-tenant` Phase 6; the
design fork is coord finding `e28caba2-9727-494a-91a0-4a321c377cb3`).
`coord_bind_repo_tenant` / `coord_unbind_repo_tenant` move a repository's
governance — and with it who coord mints that repo's push and clone tokens
for — from one tenant to another. They are onboarding-family in substance,
named outside the `coord_onboard` prefix on purpose so the sessions coord's
bind door admits can reach them. What a LEAKED device nonce reaches through
them: this proxy presents the paired device JWT, which passes coord's
possession check, so possession is NOT the limit here. The limits are
coord's: the device's USER must hold `admin` in BOTH the losing and the
gaining tenant, the credential's own tenant must be one of the two, coord
itself writes an audit row per tenant and a `repo_bound` / `repo_unbound`
operator notification on every call, and either side undoes it with one
call. Residual risk: tokens minted for the repo while a wrong binding stood
are not taken back by the unbind. This differs from
`coord_request_expedited_land`, withheld because a land cannot be undone and
its only gate is that same possession check. `coord_enroll_repo` stays
withheld as an onboarding-family name (its HTTP twin remains the door); the
binding writes are let through because they carry a both-tenants admin
check and a server-written audit and notification that no other
onboarding-family write has — that is the security rationale the DELIBERATELY EXCLUDED note
(copied into each `../deliberate-exclusions/` entry) asks for. As of this entry coord does not serve the three binding
tools yet; they are forwarded ahead, which `README.md` in this directory says is harmless.

Plan 2026-10-17-per-repo-tenant-binding-overrides-the-org-tenant Phase 6:
the per-repo tenant binding door, forwarded ahead of coord serving it.
coord will authorize it (the device user must be admin in BOTH tenants);
this list only forwards. Risk and limits: the paragraph above.
