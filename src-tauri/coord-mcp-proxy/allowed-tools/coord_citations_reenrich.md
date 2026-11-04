`coord_citations_reenrich` is IN because it is a SHIPPED REPAIR ROUTE, and a
repair route an agent cannot reach is the defect it was built to close (plan
`2026-09-10-citation-reenrich-is-unrunnable-and-plan-pr-is-unclassifiable`).
It runs coord's citation re-enrich pass — re-hydrating `touched_files` for
cited PRs from GitHub's PR-files API — and that work is mechanical,
idempotent (stamped rows are skipped), tenant-scoped and bounded (optional
`limit`, Background outbound budget). Served policy `production-and-cost`
`shipped-repair-routes-are-pipeline-behavior` makes running such a pass
ordinary pipeline behavior, and `operating-rules`
`operator-asks-are-decisions-never-api-calls` rules out routing it through
the operator instead. Withheld here, it would answer `-32601` from inside the
product and recreate exactly the state the plan names: a pass no agent could
run. coord's own grant (`mcp/agent_tool_access.rs`) is the authority on who
may call it; this list only forwards.
