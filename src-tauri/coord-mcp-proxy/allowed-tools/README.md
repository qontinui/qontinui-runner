# allowed-tools

Coord MCP tools a proxied session may invoke (credential-hygiene Task 4).

Same posture as the sibling [`ClaimsReadTarget`] / [`CoordWriteTarget`]
enums: the per-session nonce authenticates a *session*, not an operator,
so its authority is an ENUMERATED set — the legitimate session-coordination
surface (introspection reads + the coordination writes: declare-intent,
work-unit upsert/transition, gate register/attest, claims, expectations,
findings/messaging/memory, orient/status/conflict-check/blockers). A leaked
nonce must not reach anything beyond that with the runner's device
identity. Coord is being hardened server-side in parallel; this list is
defense-in-depth, not the sole authority.

Forwarding a name coord does not yet serve is harmless — coord answers it as
an unknown tool — so these entries may land ahead of the coord side.

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
