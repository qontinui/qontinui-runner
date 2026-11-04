The same diff of coord's `DEVICE_DEFAULT_TOOLS` / `TWIN_READ_TOOLS` against
this list (coord `mcp/agent_tool_access.rs` on origin/main, 2026-09-29) found
ten more names that were neither allowed nor deliberately excluded — the
drift state `coord_mcp_filter_tools_list_response` warns about. Each is
granted on coord's device floor with a recorded rationale. Eight of them
also have a device-admitting HTTP twin (a `DoorAdmits::DeviceAgent` row that
coord's `device_floor_matches_device_admitting_http_doors` pins), so
withholding those here was a transport asymmetry rather than a boundary. The
other two have no such twin and rest on coord's own grant alone:
`coord_fleet_drain_status` is on coord's read-only observer floor, and
`coord_land_provenance_backfill` is a deliberately bounded agent door onto a
repair pass whose HTTP route is operator-only (coord plan
`2026-09-15-two-admin-only-repair-passes-have-no-agent-door`). That is the
same precedent as `coord_citations_reenrich` (`coord_citations_reenrich.md`): coord's grant is the
authority on who may call it, and this list only forwards.

* reads — `coord_fleet_drain_status` (a session that cannot see a drain
  reads a quiesced machine as idle), `coord_next_step_settings_effective`
  (the dial governing the caller's OWN autonomy), `coord_pending_agent_questions`,
  `coord_primary_tree_branch_status`, `coord_work_unit_overview`;
* self-scoped or bounded writes — `coord_adopt_pr` (the PR adoption claim a
  fixer takes before acting on a PR it did not author),
  `coord_answer_agent_question` / `coord_withdraw_agent_question` (the
  agent-audience queue's only consumers; neither can write `audience` or
  erase an operator answer), `coord_operator_touch_classify` (append-only
  classification sidecar), `coord_land_provenance_backfill` (dry-run by
  default, precedence-aware re-derive, reversible by re-running).
