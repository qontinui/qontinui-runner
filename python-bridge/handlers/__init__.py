"""Runner-side handlers for the qontinui-web ``runner_command_ws`` bridge.

Each submodule exposes one or more ``async def handle_<command>(payload: dict) -> dict``
coroutines that dispatch a WS bridge command to the local qontinui
runtime. The Rust side (``mcp/backend_relay.rs``) invokes them by name
via :mod:`handlers.dispatch` (one Python subprocess per command), JSON
in / JSON out.

See ``plans/2026-05-17-web-runner-ws-bridge-plan-b.md`` for the
disposition rationale + the per-phase command mapping.
"""
