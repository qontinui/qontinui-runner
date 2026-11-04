# coord-mcp proxy policy lists

The loopback `/coord-mcp` proxy's compiled policy, one file per entry:

| Directory | Generated const | What it does |
|---|---|---|
| `allowed-methods/` (walked recursively: `notifications/cancelled` is `allowed-methods/notifications/cancelled.md`) | `COORD_MCP_ALLOWED_METHODS` | JSON-RPC methods forwarded |
| `allowed-tools/` | `COORD_MCP_ALLOWED_TOOLS` | `tools/call` names forwarded |
| `allowed-tool-prefixes/` | `COORD_MCP_ALLOWED_TOOL_PREFIXES` | read-only tool families forwarded by prefix |
| `deliberate-exclusions/` | `COORD_MCP_DELIBERATE_EXCLUSIONS`, and `COORD_MCP_DELIBERATE_EXCLUSION_DOORS` from each file's optional `door:` front-matter line | tools withheld ON PURPOSE (reporting only; grants and refuses nothing) |
| `deliberate-exclusion-prefixes/` | `COORD_MCP_DELIBERATE_EXCLUSION_PREFIXES` | withheld families by prefix |

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

`build_drift.rs` reads trunk's copy of these directories with one
`git ls-tree -r` to classify a `-32601` refusal's `cause`, so the directory
layout is part of the runner's contract with its own older and newer builds:
rename a directory and every refusal on every box degrades to
`cause: "unknown"` until the reader follows.

A `door:` front-matter line, when present, is the file's first block:

```
---
door: <the sanctioned alternative door a -32601 names>
---
```
