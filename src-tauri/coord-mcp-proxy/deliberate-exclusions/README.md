# deliberate-exclusions

The tools this door withholds ON PURPOSE — the "DELIBERATELY EXCLUDED"
families from [`COORD_MCP_ALLOWED_TOOLS`]'s note, restated as data.

The note has been the only record of that intent, which left withholding
with two indistinguishable causes: a security decision someone made, or this
list quietly falling behind coord's grant. The first three instances of the
latter — `coord_write_prompt_document`, `coord_agent_registry_effective`,
`coord_memory_overview` — were each found by a human hitting `-32601` and
going looking, which is why they arrived one at a time over months. Diffing
coord's grant against this list found the remaining two in one pass, which
is the argument for a check that runs on its own.

Naming the intended exclusions makes the remainder meaningful: a tool coord
grants this device that is neither allowed nor listed here is, by
construction, drift. [`coord_mcp_filter_tools_list_response`] sees exactly
that set on every `tools/list`, so the next instance announces itself at the
door instead of waiting for someone to trip over it.

This grants nothing and refuses nothing — [`coord_mcp_tool_is_allowed`]
remains the sole gate. Membership here only changes how a withholding is
REPORTED.

For a deliberate exclusion that HAS a sanctioned door elsewhere, the door a
`-32601` names instead of the generic "reach coord's /mcp directly". A
mechanical pointer, not authority: coord enforces its own checks on that
door (`security-and-autonomy` content trigger 6 — the denial names an
alternative path, deliberately).

One file per entry. The file name, minus `.md`, IS the entry; the body is its
rationale. `src-tauri/build.rs` (`generate_coord_mcp_lists`) walks this
directory, refuses a name that is not `^[a-z0-9_/.-]+$`, sorts by byte and
emits the list into `$OUT_DIR/coord_mcp_lists.rs`, which
`src-tauri/src/mcp_api.rs` `include!`s. This `README.md` is ignored by the
generator; it holds the notes that are about the list as a whole.

Adding an entry adds one new file and edits no shared line, so two PRs that
add different entries never conflict (plan
`2026-11-04-runner-generated-and-ledger-files-are-merge-neutral-and-land-behind-a-drain`
Phase 3a).

A file may open with a `door:` front-matter block (see `../README.md`); `build.rs` emits those as `COORD_MCP_DELIBERATE_EXCLUSION_DOORS`.
