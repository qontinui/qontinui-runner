"""Check if required system tools are installed."""

from __future__ import annotations

import asyncio
import logging
import shutil
import subprocess
from typing import Any

logger = logging.getLogger(__name__)

TOOLS: dict[str, dict[str, str]] = {
    "node": {
        "command": "node",
        "version_flag": "--version",
        "description": "Node.js runtime",
    },
    "python": {
        "command": "python",
        "version_flag": "--version",
        "description": "Python runtime",
    },
    "rust": {
        "command": "rustc",
        "version_flag": "--version",
        "description": "Rust compiler",
    },
    "cargo": {
        "command": "cargo",
        "version_flag": "--version",
        "description": "Rust package manager",
    },
    "git": {
        "command": "git",
        "version_flag": "--version",
        "description": "Git version control",
    },
    "npm": {
        "command": "npm",
        "version_flag": "--version",
        "description": "Node package manager",
    },
    "yarn": {
        "command": "yarn",
        "version_flag": "--version",
        "description": "Yarn package manager",
    },
    "poetry": {
        "command": "poetry",
        "version_flag": "--version",
        "description": "Python dependency manager",
    },
    "claude_cli": {
        "command": "claude",
        "version_flag": "--version",
        "description": "Claude CLI",
    },
    "gemini_cli": {
        "command": "gemini",
        "version_flag": "--version",
        "description": "Gemini CLI",
    },
    "docker": {
        "command": "docker",
        "version_flag": "--version",
        "description": "Docker",
    },
}


def _parse_version(raw: str) -> str | None:
    """Extract a clean version string from command output.

    Takes the first line and strips common prefixes like 'v', 'Python ', etc.
    """
    if not raw or not raw.strip():
        return None
    first_line = raw.strip().splitlines()[0].strip()
    if not first_line:
        return None
    return first_line


def _check_single_tool(tool_key: str, tool_info: dict[str, str]) -> dict[str, Any]:
    """Synchronously check a single tool's availability and version."""
    command = tool_info["command"]
    version_flag = tool_info["version_flag"]
    description = tool_info["description"]

    result: dict[str, Any] = {
        "tool": tool_key,
        "installed": False,
        "version": None,
        "path": None,
        "description": description,
    }

    path = shutil.which(command)
    if path is None:
        return result

    result["path"] = path

    try:
        proc = subprocess.run(
            [command, version_flag],
            capture_output=True,
            text=True,
            timeout=5,
        )
        # Some tools print version to stderr (e.g., rustc, java)
        raw_output = proc.stdout or proc.stderr
        version = _parse_version(raw_output)
        if version:
            result["installed"] = True
            result["version"] = version
        else:
            # Tool found on PATH but produced no version output — still
            # treat as installed since shutil.which found it.
            result["installed"] = True
    except FileNotFoundError:
        # shutil.which found it but it can't actually be executed
        logger.debug("Tool %s found at %s but could not be executed", command, path)
    except subprocess.TimeoutExpired:
        # Tool exists but hung getting version — mark installed with no version
        logger.debug("Tool %s timed out getting version", command)
        result["installed"] = True
    except OSError as exc:
        logger.debug("OS error checking tool %s: %s", command, exc)

    return result


async def check_prerequisites(checks: list[str] | None = None) -> dict[str, Any]:
    """Check if system tools are installed.

    Args:
        checks: list of tool keys to check (must be keys in ``TOOLS``).
            If *None*, all known tools are checked.

    Returns:
        A dict with:
            ``results`` — list of per-tool dicts with keys *tool*, *installed*,
            *version*, *path*, *description*.
            ``summary`` — dict with *total*, *installed*, *missing* counts.
    """
    if checks is not None:
        # Filter to only requested tools, silently skip unknown keys
        tools_to_check = {k: v for k, v in TOOLS.items() if k in checks}
    else:
        tools_to_check = TOOLS

    loop = asyncio.get_running_loop()

    # Run all checks concurrently via the thread pool
    tasks = {
        key: loop.run_in_executor(None, _check_single_tool, key, info)
        for key, info in tools_to_check.items()
    }

    results: list[dict[str, Any]] = []
    for key in tools_to_check:
        result = await tasks[key]
        results.append(result)

    installed_count = sum(1 for r in results if r["installed"])
    total = len(results)

    return {
        "results": results,
        "summary": {
            "total": total,
            "installed": installed_count,
            "missing": total - installed_count,
        },
    }
