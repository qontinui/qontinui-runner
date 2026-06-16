"""Log source CRUD operations via the runner API.

All mutating operations use a read-modify-write pattern against the
full log-source settings object (GET → modify → PUT).
"""

from __future__ import annotations

import logging
import uuid
from typing import Any

from qontinui_setup_mcp.client import RunnerClient

logger = logging.getLogger(__name__)


async def get_log_sources(client: RunnerClient) -> dict[str, Any]:
    """Get all configured log sources.

    Returns the full settings object containing ``sources``, ``profiles``,
    ``default_profile_id``, ``ai_selection_mode``, and
    ``include_all_when_no_profile``.
    """
    resp = await client.get_log_source_settings()
    if not resp.success:
        return {
            "success": False,
            "error": resp.error or "Failed to fetch log source settings",
        }
    return {"success": True, **resp.data}


async def add_log_source(
    client: RunnerClient, source: dict[str, Any]
) -> dict[str, Any]:
    """Add a new log source via read-modify-write.

    If the *source* dict does not contain an ``"id"`` key, a UUID4 is
    generated automatically.

    Returns:
        ``{"success": True, "source_id": "<id>"}`` on success, or
        ``{"success": False, "error": "..."}`` on failure.
    """
    # Ensure the source has an id.
    if "id" not in source or not source["id"]:
        source["id"] = str(uuid.uuid4())
    source_id: str = source["id"]

    # Read current settings.
    resp = await client.get_log_source_settings()
    if not resp.success:
        return {
            "success": False,
            "error": resp.error or "Failed to read current settings",
        }

    settings: dict[str, Any] = resp.data

    # Append the new source.
    sources: list[dict[str, Any]] = settings.get("sources", [])
    sources.append(source)
    settings["sources"] = sources

    # Write back.
    put_resp = await client.put_log_source_settings(settings)
    if not put_resp.success:
        return {"success": False, "error": put_resp.error or "Failed to save settings"}

    return {"success": True, "source_id": source_id}


async def update_log_source(
    client: RunnerClient,
    source_id: str,
    updates: dict[str, Any],
) -> dict[str, Any]:
    """Update an existing log source by ID via read-modify-write.

    Merges *updates* into the existing source dict.  The ``"id"`` field
    cannot be changed.

    Returns:
        ``{"success": True}`` on success, or
        ``{"success": False, "error": "..."}`` on failure.
    """
    resp = await client.get_log_source_settings()
    if not resp.success:
        return {
            "success": False,
            "error": resp.error or "Failed to read current settings",
        }

    settings: dict[str, Any] = resp.data
    sources: list[dict[str, Any]] = settings.get("sources", [])

    # Find the source to update.
    target: dict[str, Any] | None = None
    for src in sources:
        if src.get("id") == source_id:
            target = src
            break

    if target is None:
        return {"success": False, "error": f"Log source '{source_id}' not found"}

    # Merge updates (preserve the id).
    updates.pop("id", None)
    target.update(updates)

    settings["sources"] = sources

    put_resp = await client.put_log_source_settings(settings)
    if not put_resp.success:
        return {"success": False, "error": put_resp.error or "Failed to save settings"}

    return {"success": True}


async def remove_log_source(client: RunnerClient, source_id: str) -> dict[str, Any]:
    """Remove a log source by ID via read-modify-write.

    Also removes the source ID from any profiles that reference it.

    Returns:
        ``{"success": True}`` on success, or
        ``{"success": False, "error": "..."}`` on failure.
    """
    resp = await client.get_log_source_settings()
    if not resp.success:
        return {
            "success": False,
            "error": resp.error or "Failed to read current settings",
        }

    settings: dict[str, Any] = resp.data
    sources: list[dict[str, Any]] = settings.get("sources", [])

    original_count = len(sources)
    settings["sources"] = [s for s in sources if s.get("id") != source_id]

    if len(settings["sources"]) == original_count:
        return {"success": False, "error": f"Log source '{source_id}' not found"}

    # Remove the source from any profiles that reference it.
    profiles: list[dict[str, Any]] = settings.get("profiles", [])
    for profile in profiles:
        source_ids: list[str] = profile.get("source_ids", [])
        if source_id in source_ids:
            profile["source_ids"] = [sid for sid in source_ids if sid != source_id]

    put_resp = await client.put_log_source_settings(settings)
    if not put_resp.success:
        return {"success": False, "error": put_resp.error or "Failed to save settings"}

    return {"success": True}


async def apply_suggested_sources(
    client: RunnerClient,
    project_path: str,
    dry_run: bool = False,
) -> dict[str, Any]:
    """Discover log sources for a project and add them to the runner.

    Uses :func:`qontinui_setup_mcp.discovery.log_finder.suggest_log_sources`
    to scan *project_path* for common log locations, then adds each
    suggested source via the runner API.

    Args:
        client: The runner HTTP client.
        project_path: Filesystem path to the project root.
        dry_run: If ``True``, return what would be added without persisting.

    Returns:
        ``{"success": True, "sources_added": [...], "framework": {...}, "dry_run": bool}``
        on success, or ``{"success": False, "error": "..."}`` on failure.
    """
    try:
        from qontinui_setup_mcp.discovery.log_finder import suggest_log_sources
    except ImportError:
        return {
            "success": False,
            "error": "Log finder module not available (qontinui_setup_mcp.discovery.log_finder)",
        }

    try:
        result = await suggest_log_sources(project_path)
    except Exception as exc:
        logger.exception("Failed to discover log sources for %s", project_path)
        return {"success": False, "error": f"Discovery failed: {exc}"}

    suggested: list[dict[str, Any]] = result.get("suggested_sources", [])
    framework: dict[str, Any] = result.get("framework", {})

    if dry_run:
        return {
            "success": True,
            "sources_added": suggested,
            "framework": framework,
            "dry_run": True,
        }

    # Read current settings once, add all sources, write once.
    resp = await client.get_log_source_settings()
    if not resp.success:
        return {
            "success": False,
            "error": resp.error or "Failed to read current settings",
        }

    settings: dict[str, Any] = resp.data
    sources: list[dict[str, Any]] = settings.get("sources", [])

    added: list[dict[str, Any]] = []
    for source in suggested:
        if "id" not in source or not source["id"]:
            source["id"] = str(uuid.uuid4())
        sources.append(source)
        added.append(source)

    settings["sources"] = sources

    put_resp = await client.put_log_source_settings(settings)
    if not put_resp.success:
        return {"success": False, "error": put_resp.error or "Failed to save settings"}

    return {
        "success": True,
        "sources_added": added,
        "framework": framework,
        "dry_run": False,
    }
