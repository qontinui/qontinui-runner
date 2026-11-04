`coord_alert_queue`, `coord_alert_claim` and `coord_alert_release` are IN
because alerts are AGENTS' work (same plan, Phase 2): coord routes each alert
kind to a responder domain, and the queue is how a steward session reads its
domain's open alerts, leases one before acting so two sessions do not work the
same condition, and hands the lease back. The queue is a read; claim and
release write only a lease on the alert row — claiming never RESOLVES
(resolution stays with coord's own re-observation), so an agent cannot mark
fixed a condition coord still sees. Coord grants all three on the device floor
(`agent_tool_access.rs` `DEVICE_DEFAULT_TOOLS`); withheld here, the queue the
phase builds would be unreachable from exactly the sessions it exists for.
