`coord_fleet_dispatch_roles` (qontinui-coord#3015, plan
`2026-10-02-fleet-machine-roles-workhorse-bench-ci-node` Phase 3) is the
read-only twin of `GET /coord/fleet/dispatch-roles`: every machine of the
caller's tenant with its dispatch role, per-lane state with the role and
drain layers shown separately, and the role suggestion. That PR landed on
coord `main` (`a5962a19b..602279acd`) granting it on coord's
`READ_ONLY_TOOLS`, `MERGER_TOOLS` and `DEVICE_DEFAULT_TOOLS`; later commits
added `OVERLORD_TOOLS` (`1edd47702`) and `BOOTSTRAP_READ_TOOLS`
(`6b604d803`). As of coord `origin/main` `b8ac37b6b` (read 2026-10-18) it is
on all five (`crates/coord/src/mcp/agent_tool_access.rs`), each beside
`coord_fleet_drain_status`, for the same reason: a session that cannot see
a machine's role reads a bench or CI node as an idle workhorse. Its WRITE
(`PUT /coord/fleet/dispatch-role`) is operator-only there and has no MCP
tool, so there is nothing to exclude. Re-verify against coord
`origin/main` when editing this paragraph.
