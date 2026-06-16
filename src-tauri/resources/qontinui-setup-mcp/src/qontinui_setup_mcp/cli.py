"""CLI entry point for subprocess invocation by the Rust runner.

Provides a thin CLI wrapper around the offline discovery and validation
functions so that the runner can call them via ``python -m qontinui_setup_mcp.cli``
without needing the full MCP server or an HTTP connection.

Usage::

    python -m qontinui_setup_mcp.cli scan_workspace /path/to/dir --max-depth 3
    python -m qontinui_setup_mcp.cli detect_framework /path/to/project
    python -m qontinui_setup_mcp.cli suggest_log_sources /path/to/project
    python -m qontinui_setup_mcp.cli check_prerequisites
    python -m qontinui_setup_mcp.cli check_prerequisites --checks node python git

Exit codes:
    0 — success (JSON result on stdout)
    1 — error (message on stderr)
"""

from __future__ import annotations

import argparse
import asyncio
import json
import sys
from typing import Any


def _build_parser() -> argparse.ArgumentParser:
    """Build the top-level argument parser with subcommands."""
    parser = argparse.ArgumentParser(
        prog="qontinui_setup_mcp.cli",
        description="Qontinui setup utilities — offline discovery and validation.",
    )
    subparsers = parser.add_subparsers(dest="command", required=True)

    # -- scan_workspace --------------------------------------------------------
    sp_scan = subparsers.add_parser(
        "scan_workspace",
        help="Scan a directory tree for software projects.",
    )
    sp_scan.add_argument(
        "path",
        help="Root directory to scan.",
    )
    sp_scan.add_argument(
        "--max-depth",
        type=int,
        default=3,
        help="Maximum directory depth to descend (default: 3).",
    )

    # -- detect_framework ------------------------------------------------------
    sp_detect = subparsers.add_parser(
        "detect_framework",
        help="Detect the primary framework of a project.",
    )
    sp_detect.add_argument(
        "project_path",
        help="Path to the project root.",
    )

    # -- suggest_log_sources ---------------------------------------------------
    sp_suggest = subparsers.add_parser(
        "suggest_log_sources",
        help="Discover log files and suggest log source configs.",
    )
    sp_suggest.add_argument(
        "project_path",
        help="Path to the project root.",
    )

    # -- suggest_workspace_sources ---------------------------------------------
    sp_ws = subparsers.add_parser(
        "suggest_workspace_sources",
        help="Discover dev-log sources at the workspace root level.",
    )
    sp_ws.add_argument(
        "workspace_path",
        help="Root directory of the workspace.",
    )

    # -- check_prerequisites ---------------------------------------------------
    sp_prereqs = subparsers.add_parser(
        "check_prerequisites",
        help="Check if required system tools are installed.",
    )
    sp_prereqs.add_argument(
        "--checks",
        nargs="+",
        default=None,
        help="Specific tools to check (e.g. node python git). Omit to check all.",
    )

    return parser


async def _dispatch(args: argparse.Namespace) -> Any:
    """Dispatch the parsed CLI arguments to the appropriate async function."""
    command: str = args.command

    if command == "scan_workspace":
        from qontinui_setup_mcp.discovery.scanner import scan_workspace

        return await scan_workspace(path=args.path, max_depth=args.max_depth)

    if command == "detect_framework":
        from qontinui_setup_mcp.discovery.frameworks import detect_framework

        return await detect_framework(project_path=args.project_path)

    if command == "suggest_log_sources":
        from qontinui_setup_mcp.discovery.log_finder import suggest_log_sources

        return await suggest_log_sources(project_path=args.project_path)

    if command == "suggest_workspace_sources":
        from qontinui_setup_mcp.discovery.log_finder import suggest_workspace_sources

        return await suggest_workspace_sources(workspace_path=args.workspace_path)

    if command == "check_prerequisites":
        from qontinui_setup_mcp.validation.prerequisites import check_prerequisites

        return await check_prerequisites(checks=args.checks)

    # Should never happen due to argparse required=True, but guard anyway.
    raise ValueError(f"Unknown command: {command}")


def main() -> None:
    """Parse arguments, run the async dispatch, and print JSON to stdout."""
    parser = _build_parser()
    args = parser.parse_args()

    try:
        result = asyncio.run(_dispatch(args))
        json.dump(result, sys.stdout, indent=2, default=str)
        sys.stdout.write("\n")
        sys.exit(0)
    except Exception as exc:
        print(f"Error: {exc}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
