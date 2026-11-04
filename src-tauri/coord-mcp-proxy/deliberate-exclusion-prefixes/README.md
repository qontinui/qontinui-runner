# deliberate-exclusion-prefixes

Deliberately-excluded FAMILIES by prefix: a withheld tool whose name starts with an entry here is reported as `deliberate`, never `drift`.

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
