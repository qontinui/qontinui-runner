"""The runner HTTP API's address, as the Python bridge must dial it.

The runner binds its API on the IPv4 loopback ONLY (``RUNNER_API_BIND_IP`` in
``src-tauri/src/mcp/types.rs``). Windows resolves ``localhost`` to ``::1``
first, and Python's ``urllib`` / ``requests`` / ``aiohttp`` have no
happy-eyeballs fallback for it, so a ``http://localhost:<port>`` URL waits out a
refused IPv6 connect (~2 s, measured 2026-10-25) on every new connection.
Every runner URL the bridge builds therefore comes from here.

The port is the one the runner hands this subprocess in ``QONTINUI_PORT``, so a
secondary or temp runner (9877+) is addressed correctly instead of the primary.

Plan ``2026-10-25-runner-self-calls-dial-localhost-and-pay-a-doomed-ipv6-connect``.
"""

import os

RUNNER_API_HOST = "127.0.0.1"
DEFAULT_RUNNER_API_PORT = 9876


def runner_api_port() -> int:
    """The runner API port: ``QONTINUI_PORT`` when set, else 9876."""
    return int(os.environ.get("QONTINUI_PORT", str(DEFAULT_RUNNER_API_PORT)))


def runner_api_base() -> str:
    """The runner API base URL, e.g. ``http://127.0.0.1:9877``. No trailing slash."""
    return f"http://{RUNNER_API_HOST}:{runner_api_port()}"
