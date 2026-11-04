# allowed-methods

JSON-RPC methods the loopback `/coord-mcp` proxy forwards (credential-
hygiene Task 4). Everything else is rejected BEFORE any bearer is read or
any byte reaches coord. The MCP handshake + tool surface is exactly:
`initialize` / `notifications/*` housekeeping / `ping` / `tools/list` /
`tools/call` — and `tools/call` is further gated per tool name by
[`coord_mcp_tool_is_allowed`]. Sorted for `binary_search`.

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

This directory is walked recursively: an entry is its path relative to this directory minus `.md`, so `notifications/cancelled` is `notifications/cancelled.md`.
