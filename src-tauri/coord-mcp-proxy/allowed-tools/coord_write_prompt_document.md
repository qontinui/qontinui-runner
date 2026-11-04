`coord_write_prompt_document` is IN, and the reason it used to be out no longer
holds. The old rationale — "policy authorship is tenant-gated and belongs to an
operator, not a device nonce" — described a coord that no longer exists: coord
deliberately grants this tool to device/agent principals
(`coord::mcp::agent_tool_access`) precisely so a session can CLOSE a POLICY_GAP
it found instead of recording one that waits on the operator, which was the
bottleneck that made gaps accumulate faster than they could be hand-published.
Withholding it HERE did not re-impose an operator gate; it silently removed a
capability coord had already decided to grant, and the denial surfaced as a bare
`-32601` that reads like "no such tool" rather than "your proxy withholds it".

Nor is the grant unguarded — coord enforces non-loosening server-side, so this
proxy is not the thing standing between a device nonce and a weakened policy:
`append` preserves the existing body verbatim, `create` only authors names that
do not yet exist, `edit_clause` lands only a provable tightening or no-op and
otherwise becomes a pending operator proposal, and the meta-policies
(`session-protocol`, `security-and-autonomy`, `escalation-bar`) are refused
outright in either direction. Tenant comes from the verified `CallerIdentity`,
never an argument; every landed write is versioned, attributed and revertible.

Keep it in. If policy authorship should ever be operator-only again, that is a
decision to make in coord's grant — one place, enforced for every transport —
not by re-diverging this list from it.
