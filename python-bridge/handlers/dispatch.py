"""One-shot WS bridge command dispatcher.

Invoked by the Rust side (``mcp/backend_relay.rs``) as a Python
subprocess for each inbound WS bridge command. Protocol:

- Argv: ``python -m handlers.dispatch <command_name>``
- Stdin: JSON-encoded payload dict (one line, terminated by ``\n``).
- Stdout: JSON-encoded response dict (one line, terminated by ``\n``).
- Stderr: logs (free-form).
- Exit code: 0 on success (response is on stdout), non-zero on
  dispatcher failure (e.g. unknown command, IO error). qontinui-side
  exceptions are caught by the handler and surfaced as a structured
  error response on stdout — exit code stays 0 for those.

The dispatcher imports its handler module lazily so adding a new
command in a follow-up phase is a one-line addition to ``_COMMANDS``
plus a new submodule.
"""

from __future__ import annotations

import asyncio
import json
import logging
import sys
import traceback
from typing import Awaitable, Callable

logger = logging.getLogger(__name__)


HandlerFn = Callable[[dict], Awaitable[dict]]


def _load_handler(command: str) -> HandlerFn:
    """Resolve a command name to its handler coroutine.

    Lazy-loads the handler module to keep startup cheap.
    """
    # The mapping is hand-maintained; each entry corresponds to one
    # qontinui_schemas.commands.* schema + one runner-side handler.
    if command == "state_machine.discover_ui_bridge":
        from handlers.state_machine import discover_ui_bridge

        return discover_ui_bridge

    raise ValueError(f"unknown command: {command!r}")


async def _main() -> int:
    logging.basicConfig(
        stream=sys.stderr,
        level=logging.INFO,
        format="%(asctime)s [%(levelname)s] handlers.dispatch: %(message)s",
    )

    if len(sys.argv) < 2:
        print(
            json.dumps(
                {
                    "error": "internal_error",
                    "message": "missing command argument",
                }
            )
        )
        return 2

    command = sys.argv[1]

    try:
        payload_raw = sys.stdin.read()
        payload: dict = json.loads(payload_raw) if payload_raw.strip() else {}
    except json.JSONDecodeError as exc:
        print(
            json.dumps(
                {
                    "error": "invalid_payload",
                    "message": f"stdin JSON parse failed: {exc!s}",
                }
            )
        )
        return 3

    try:
        handler = _load_handler(command)
    except ValueError as exc:
        print(
            json.dumps(
                {
                    "error": "unknown_command",
                    "message": str(exc),
                }
            )
        )
        return 4

    try:
        response = await handler(payload)
    except Exception as exc:  # noqa: BLE001 - intentionally broad
        logger.exception("dispatcher: handler raised uncaught exception")
        print(
            json.dumps(
                {
                    "error": "internal_error",
                    "message": str(exc),
                    "traceback": traceback.format_exc(),
                }
            )
        )
        return 5

    print(json.dumps(response))
    return 0


if __name__ == "__main__":
    sys.exit(asyncio.run(_main()))
