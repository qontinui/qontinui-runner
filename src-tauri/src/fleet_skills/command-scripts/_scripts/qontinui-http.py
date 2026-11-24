#!/usr/bin/env python3
"""Thin CLI wrapper for Qontinui Runner HTTP client.

This script provides a simple command-line interface to the QontinuiClient
from qontinui-mcp. It has minimal code - all the heavy lifting is done by
the qontinui_mcp.client module.

Usage:
    python qontinui-http.py status
    python qontinui-http.py load-config /path/to/config.json
    python qontinui-http.py run-workflow "WorkflowName"
    python qontinui-http.py monitors
    python qontinui-http.py load-last-config

Note: This requires qontinui-mcp to be installed:
    cd qontinui-mcp && pip install -e .
"""

from __future__ import annotations

import argparse
import asyncio
import json
import sys

try:
    from qontinui_mcp.client import QontinuiClient, DEFAULT_RUNNER_PORT, EXECUTION_TIMEOUT
except ImportError as exc:
    print(f"ERROR: qontinui-mcp not importable: {exc}", file=sys.stderr)
    print("Install it from a qontinui-mcp source checkout (it is not on PyPI):", file=sys.stderr)
    print("  cd qontinui-mcp && pip install -e .", file=sys.stderr)
    sys.exit(1)


async def main() -> int:
    """CLI entry point."""
    parser = argparse.ArgumentParser(
        description="Command-line client for Qontinui Runner (thin wrapper for qontinui-mcp)"
    )
    parser.add_argument(
        "--host",
        help="Runner host (default: $QONTINUI_RUNNER_HOST, else the client's default)",
    )
    parser.add_argument(
        "--port",
        type=int,
        help=f"Runner port (default: $QONTINUI_RUNNER_PORT, else {DEFAULT_RUNNER_PORT})",
    )

    subparsers = parser.add_subparsers(dest="command", required=True)

    # status
    subparsers.add_parser("status", help="Get runner status")

    # health
    subparsers.add_parser("health", help="Health check")

    # monitors
    subparsers.add_parser("monitors", help="List monitors")

    # load-config
    load_parser = subparsers.add_parser("load-config", help="Load workflow configuration")
    load_parser.add_argument("config_path", help="Path to JSON config file")

    # run-workflow
    run_parser = subparsers.add_parser("run-workflow", help="Run a workflow")
    run_parser.add_argument("workflow_name", help="Name of workflow to run")
    run_parser.add_argument("--monitor", help="Monitor: left, right, primary, or index")
    run_parser.add_argument(
        "--timeout",
        type=int,
        default=EXECUTION_TIMEOUT,
        help="Timeout in seconds",
    )

    # stop
    subparsers.add_parser("stop", help="Stop current execution")

    # load-last-config
    subparsers.add_parser(
        "load-last-config",
        help="Reload the last config the runner saved (reports its saved workflow and monitor)",
    )

    args = parser.parse_args()

    # Create client. The port is passed only when given, so the client applies
    # QONTINUI_RUNNER_PORT and its own default exactly as it does for the host.
    port_arg = {} if args.port is None else {"port": args.port}
    client = QontinuiClient(host=args.host, **port_arg)

    try:
        # Execute command
        if args.command == "status":
            response = await client.status()
            result = {
                "success": response.success,
                "data": response.data,
                "error": response.error,
            }
        elif args.command == "health":
            response = await client.health()
            result = {
                "success": response.success,
                "data": response.data,
                "error": response.error,
            }
        elif args.command == "monitors":
            response = await client.list_monitors()
            result = {
                "success": response.success,
                "data": response.data,
                "error": response.error,
            }
        elif args.command == "load-config":
            response = await client.load_config(args.config_path)
            result = {
                "success": response.success,
                "data": response.data,
                "error": response.error,
            }
        elif args.command == "run-workflow":
            exec_result = await client.run_workflow(
                args.workflow_name,
                monitor=args.monitor,
                timeout=float(args.timeout),
            )
            result = {
                "success": exec_result.success,
                "data": {
                    "execution_id": exec_result.execution_id,
                    "success": exec_result.success,
                    "duration_ms": exec_result.duration_ms,
                    "error": exec_result.error,
                    "events": exec_result.events,
                },
            }
        elif args.command == "stop":
            response = await client.stop_execution()
            result = {
                "success": response.success,
                "data": response.data,
                "error": response.error,
            }
        elif args.command == "load-last-config":
            # The runner serves POST /load-last-config: it reloads the last
            # config from its settings and reports the workflow and monitor it
            # saved (it selects neither). QontinuiClient has no wrapper for it,
            # so go through its request helper.
            response = await client._request("POST", "/load-last-config")
            result = {
                "success": response.success,
                "data": response.data,
                "error": response.error,
            }
        else:
            print(f"Unknown command: {args.command}", file=sys.stderr)
            return 1

        # Print result
        print(json.dumps(result, indent=2))
        return 0 if result.get("success", False) else 1

    finally:
        await client.close()


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
