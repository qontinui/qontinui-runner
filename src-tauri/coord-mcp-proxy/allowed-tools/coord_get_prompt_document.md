LOAD-BEARING, do not strip as "reads nobody uses": `coord_list_prompt_documents`
and `coord_get_prompt_document` are how the fleet reads served POLICY, and the
`/policy` skill's transport cascade calls them through THIS proxy (a
`tools/call` POST to `/coord-mcp` with the `.mcp.json` nonce). Every session is
required to read policy before substantive work, so omitting them does not fail
loudly — `/policy` silently falls through to its last-resort `qontinui-dev-notes`
mirrors and the fleet starts booting off stale policy with nothing surfaced.
`coord_gate_list` / `coord_withdraw_gate` are the same story for `/gate-sweep`
and `/gate`, which already document driving them over this proxy.
