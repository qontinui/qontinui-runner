`coord_agent_registry_effective` is IN, and it is the same divergence one step
further on. It is a read-only, tenant-scoped fold of (registry row × user
preference) — it records nothing and changes no preference — and it is the ONLY
way to tell a genuinely recorded `degrade` from a `degrade` assumed because the
read failed. Policy binds a gate to exactly that distinction:
`pre-pr-review` makes the user's recorded disposition govern how the review gate
is satisfied, and `registry-unreadable-falls-back-to-degrade` says an unreadable
registry is UNKNOWN and must fall back to the degraded inline self-review.

Withholding it here did not add a gate — it made one permanently unreachable.
Every device/agent session was forced onto the degraded path and could never
observe an `enabled` disposition, so a policy-required `code-reviewer` pass was
silently downgraded to an author reviewing their own diff, for every such session,
regardless of what the user had actually recorded. That is the precise failure
`harness-injected-agent-prohibitions` exists to prevent, arriving by a different
road. Same shape as `coord_write_prompt_document` (`coord_write_prompt_document.md`): the denial surfaced as a
bare `-32601` that reads like "no such tool" rather than "your proxy withholds it".

And as with that tool, coord had ALREADY decided the other way — this list was the
only thing still dissenting. `coord::mcp::agent_tool_access` grants the tool to
device/agent principals and registers its HTTP twin
`GET /coord/agent-registry/effective` as `DoorAdmits::DeviceAgent` over the shared
`agent_registry::resolve_effective`, with the tenant floor enforced inside that
shared function rather than at either door. Coord's own note on the entry reaches
this same conclusion in its own words: "Masking it was not a boundary, it was a
self-inflicted blind spot … The session bound BY the setting was the one principal
that could not read it." A device JWT could always get this answer over HTTP; the
proxy simply refused the MCP door to the same data.

Reading the registry grants no authority to SPAWN. `agent-spawn-authorization`
still governs that, and a genuine user deselection remains a never — this only
lets a session find out which it is.
