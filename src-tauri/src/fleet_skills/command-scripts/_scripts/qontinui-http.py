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

    # Checkpoint commands (SQLite database)
    python qontinui-http.py checkpoint-get improve-all
    python qontinui-http.py checkpoint-save improve-all --phase 3 --total 12
    python qontinui-http.py checkpoint-delete improve-all
    python qontinui-http.py checkpoint-list
    python qontinui-http.py checkpoint-status improve-all
    python qontinui-http.py checkpoint-history --workflow improve-all

Note: This requires qontinui-mcp to be installed:
    cd qontinui-mcp && pip install -e .
"""

from __future__ import annotations

import argparse
import asyncio
import json
import sys

import httpx

try:
    from qontinui_mcp.client import QontinuiClient, DEFAULT_RUNNER_PORT, EXECUTION_TIMEOUT
except ImportError:
    print("ERROR: qontinui-mcp not found. Please install it:", file=sys.stderr)
    print("  cd qontinui-mcp && pip install -e .", file=sys.stderr)
    sys.exit(1)


# Direct HTTP functions for checkpoint API (not in qontinui-mcp yet)
async def checkpoint_get(host: str, port: int, name: str) -> dict:
    """Get a checkpoint by workflow name."""
    async with httpx.AsyncClient() as client:
        url = f"http://{host}:{port}/checkpoints/{name}"
        response = await client.get(url)
        return {"success": response.status_code == 200, "data": response.json()}


async def checkpoint_save(
    host: str,
    port: int,
    workflow_name: str,
    current_phase: int,
    total_phases: int | None = None,
    completed: bool = False,
    restart_permitted: bool = True,
    run_id: str | None = None,
) -> dict:
    """Save/update a checkpoint."""
    async with httpx.AsyncClient() as client:
        url = f"http://{host}:{port}/checkpoints"
        data = {
            "workflow_name": workflow_name,
            "current_phase": current_phase,
            "completed": completed,
            "restart_permitted": restart_permitted,
        }
        if total_phases is not None:
            data["total_phases"] = total_phases
        if run_id is not None:
            data["run_id"] = run_id
        response = await client.post(url, json=data)
        return {"success": response.status_code == 200, "data": response.json()}


async def checkpoint_delete(host: str, port: int, name: str) -> dict:
    """Delete a checkpoint by workflow name."""
    async with httpx.AsyncClient() as client:
        url = f"http://{host}:{port}/checkpoints/{name}"
        response = await client.delete(url)
        return {"success": response.status_code == 200, "data": response.json()}


async def checkpoint_list(host: str, port: int) -> dict:
    """List all active checkpoints."""
    async with httpx.AsyncClient() as client:
        url = f"http://{host}:{port}/checkpoints"
        response = await client.get(url)
        return {"success": response.status_code == 200, "data": response.json()}


async def checkpoint_status(
    host: str, port: int, name: str, completion_value: int = 12
) -> dict:
    """Check checkpoint status for cross-session continuation."""
    async with httpx.AsyncClient() as client:
        url = f"http://{host}:{port}/checkpoints/{name}/status"
        params = {"completion_value": completion_value}
        response = await client.get(url, params=params)
        return {"success": response.status_code == 200, "data": response.json()}


async def checkpoint_history(
    host: str, port: int, workflow_name: str | None = None, limit: int = 50
) -> dict:
    """Get checkpoint/session history."""
    async with httpx.AsyncClient() as client:
        url = f"http://{host}:{port}/checkpoints/history"
        params = {"limit": limit}
        if workflow_name:
            params["workflow_name"] = workflow_name
        response = await client.get(url, params=params)
        return {"success": response.status_code == 200, "data": response.json()}


async def main() -> int:
    """CLI entry point."""
    parser = argparse.ArgumentParser(
        description="Command-line client for Qontinui Runner (thin wrapper for qontinui-mcp)"
    )
    parser.add_argument("--host", help="Runner host (default: auto-detect from WSL)")
    parser.add_argument(
        "--port",
        type=int,
        default=DEFAULT_RUNNER_PORT,
        help=f"Runner port (default: {DEFAULT_RUNNER_PORT})",
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
        help="Load the last used configuration from settings",
    )

    # Checkpoint commands (SQLite database)
    # checkpoint-get
    cp_get = subparsers.add_parser("checkpoint-get", help="Get checkpoint by workflow name")
    cp_get.add_argument("name", help="Workflow name (e.g., 'improve-all')")

    # checkpoint-save
    cp_save = subparsers.add_parser("checkpoint-save", help="Save/update checkpoint")
    cp_save.add_argument("name", help="Workflow name (e.g., 'improve-all')")
    cp_save.add_argument("--phase", type=int, required=True, help="Current phase number")
    cp_save.add_argument("--total", type=int, help="Total phases")
    cp_save.add_argument("--completed", action="store_true", help="Mark as completed")
    cp_save.add_argument(
        "--no-restart", action="store_true", help="Disable restart permission"
    )
    cp_save.add_argument("--run-id", help="Run ID for grouping sessions")

    # checkpoint-delete
    cp_del = subparsers.add_parser("checkpoint-delete", help="Delete checkpoint")
    cp_del.add_argument("name", help="Workflow name to delete")

    # checkpoint-list
    subparsers.add_parser("checkpoint-list", help="List all active checkpoints")

    # checkpoint-status
    cp_status = subparsers.add_parser(
        "checkpoint-status", help="Check checkpoint completion status"
    )
    cp_status.add_argument("name", help="Workflow name")
    cp_status.add_argument(
        "--completion-value",
        type=int,
        default=12,
        help="Phase value that indicates completion (default: 12)",
    )

    # checkpoint-history
    cp_history = subparsers.add_parser(
        "checkpoint-history", help="Get checkpoint/session history"
    )
    cp_history.add_argument("--workflow", help="Filter by workflow name")
    cp_history.add_argument("--limit", type=int, default=50, help="Max results (default: 50)")

    args = parser.parse_args()

    # Create client
    client = QontinuiClient(host=args.host, port=args.port)

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
            # Note: This endpoint doesn't exist in qontinui-mcp client yet
            # For now, return an error suggesting to use load-config instead
            result = {
                "success": False,
                "error": "load-last-config is not yet implemented. Use load-config instead.",
            }
        # Checkpoint commands (SQLite database)
        elif args.command == "checkpoint-get":
            host = args.host or client._get_host()
            result = await checkpoint_get(host, args.port, args.name)
        elif args.command == "checkpoint-save":
            host = args.host or client._get_host()
            result = await checkpoint_save(
                host,
                args.port,
                args.name,
                args.phase,
                total_phases=args.total,
                completed=args.completed,
                restart_permitted=not args.no_restart,
                run_id=args.run_id,
            )
        elif args.command == "checkpoint-delete":
            host = args.host or client._get_host()
            result = await checkpoint_delete(host, args.port, args.name)
        elif args.command == "checkpoint-list":
            host = args.host or client._get_host()
            result = await checkpoint_list(host, args.port)
        elif args.command == "checkpoint-status":
            host = args.host or client._get_host()
            result = await checkpoint_status(
                host, args.port, args.name, args.completion_value
            )
        elif args.command == "checkpoint-history":
            host = args.host or client._get_host()
            result = await checkpoint_history(
                host, args.port, workflow_name=args.workflow, limit=args.limit
            )
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
