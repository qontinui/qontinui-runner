"""Validate that configured log sources exist and are accessible."""

from __future__ import annotations

import asyncio
import logging
import os
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from qontinui_setup_mcp.client import RunnerClient

logger = logging.getLogger(__name__)

# A log source is considered "fresh" if modified within this window.
FRESHNESS_THRESHOLD_SECONDS = 24 * 60 * 60  # 24 hours


def _validate_single_source(
    source: dict[str, Any],
    check_freshness: bool,
) -> dict[str, Any]:
    """Synchronously validate a single log source on the filesystem."""
    source_id: str = source.get("id", "unknown")
    name: str = source.get("name", source_id)
    path_str: str = source.get("path", "")

    result: dict[str, Any] = {
        "id": source_id,
        "name": name,
        "path": path_str,
        "exists": False,
        "readable": False,
        "fresh": None,
        "size_bytes": None,
        "last_modified": None,
        "issues": [],
    }

    if not path_str:
        result["issues"].append("No path configured for this log source")
        return result

    p = Path(path_str)

    # --- existence ---
    if not p.exists():
        result["issues"].append(f"Path does not exist: {path_str}")
        return result
    result["exists"] = True

    # --- readability ---
    if not os.access(path_str, os.R_OK):
        result["issues"].append(f"Path is not readable: {path_str}")
        return result
    result["readable"] = True

    # --- stat info ---
    try:
        stat = p.stat()
    except OSError as exc:
        result["issues"].append(f"Could not stat path: {exc}")
        return result

    if p.is_file():
        result["size_bytes"] = stat.st_size
        if stat.st_size == 0:
            result["issues"].append("File is empty")

    mtime = stat.st_mtime
    result["last_modified"] = datetime.fromtimestamp(mtime, tz=timezone.utc).isoformat()

    # --- freshness ---
    if check_freshness:
        age_seconds = time.time() - mtime
        is_fresh = age_seconds <= FRESHNESS_THRESHOLD_SECONDS
        result["fresh"] = is_fresh
        if not is_fresh:
            hours_ago = int(age_seconds / 3600)
            result["issues"].append(
                f"File has not been modified in {hours_ago} hours "
                f"(threshold: {FRESHNESS_THRESHOLD_SECONDS // 3600}h)"
            )

    return result


async def validate_log_sources(
    client: RunnerClient,
    check_freshness: bool = True,
) -> dict[str, Any]:
    """Validate all configured log sources.

    Fetches the current log-source settings from the runner, then checks each
    source path on the local filesystem for existence, readability, freshness,
    and non-emptiness.

    Args:
        client: A connected :class:`RunnerClient` instance.
        check_freshness: When *True* (default), flag files that have not been
            modified in the last 24 hours.

    Returns:
        A dict with:
            ``sources`` — per-source validation results.
            ``summary`` — dict with *total*, *valid*, *issues* counts.
    """
    response = await client.get_log_source_settings()

    if not response.success:
        return {
            "sources": [],
            "summary": {"total": 0, "valid": 0, "issues": 0},
            "error": response.error or "Failed to retrieve log source settings",
        }

    # The data payload is expected to contain a list of source objects.
    # Support both a top-level list and a dict with a "sources" key.
    data = response.data
    if isinstance(data, dict):
        sources_list: list[dict[str, Any]] = data.get("sources", [])
    elif isinstance(data, list):
        sources_list = data
    else:
        sources_list = []

    if not sources_list:
        return {
            "sources": [],
            "summary": {"total": 0, "valid": 0, "issues": 0},
        }

    loop = asyncio.get_running_loop()

    # Validate each source concurrently in the thread pool
    tasks = [
        loop.run_in_executor(None, _validate_single_source, src, check_freshness)
        for src in sources_list
    ]
    results: list[dict[str, Any]] = await asyncio.gather(*tasks)

    valid_count = sum(1 for r in results if not r["issues"])
    issues_count = sum(1 for r in results if r["issues"])

    return {
        "sources": results,
        "summary": {
            "total": len(results),
            "valid": valid_count,
            "issues": issues_count,
        },
    }
