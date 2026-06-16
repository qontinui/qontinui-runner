"""Log source profile management via the runner API.

Profiles group log sources into named sets that can be activated
together.  Mutating operations use a read-modify-write pattern
against the full log-source settings object.
"""

from __future__ import annotations

import logging
import uuid
from typing import Any

from qontinui_setup_mcp.client import RunnerClient

logger = logging.getLogger(__name__)


async def create_log_profile(
    client: RunnerClient,
    name: str,
    source_ids: list[str],
    description: str | None = None,
) -> dict[str, Any]:
    """Create a new log source profile via read-modify-write.

    Generates a UUID4 for the profile ID, appends the profile to the
    settings, and writes back the full settings object.

    Args:
        client: The runner HTTP client.
        name: Human-readable profile name.
        source_ids: List of log source IDs to include in this profile.
        description: Optional description of the profile.

    Returns:
        ``{"success": True, "profile_id": "<id>"}`` on success, or
        ``{"success": False, "error": "..."}`` on failure.
    """
    profile_id = str(uuid.uuid4())

    # Read current settings.
    resp = await client.get_log_source_settings()
    if not resp.success:
        return {
            "success": False,
            "error": resp.error or "Failed to read current settings",
        }

    settings: dict[str, Any] = resp.data

    # Build the new profile.
    profile: dict[str, Any] = {
        "id": profile_id,
        "name": name,
        "description": description,
        "source_ids": source_ids,
    }

    # Append to profiles list.
    profiles: list[dict[str, Any]] = settings.get("profiles", [])
    profiles.append(profile)
    settings["profiles"] = profiles

    # Write back.
    put_resp = await client.put_log_source_settings(settings)
    if not put_resp.success:
        return {"success": False, "error": put_resp.error or "Failed to save settings"}

    return {"success": True, "profile_id": profile_id}


async def set_default_profile(
    client: RunnerClient,
    profile_id: str | None,
) -> dict[str, Any]:
    """Set the default active log source profile.

    Uses the runner's dedicated ``set_default_profile`` endpoint rather
    than the full read-modify-write pattern.

    Args:
        client: The runner HTTP client.
        profile_id: The profile ID to set as default, or ``None`` to clear.

    Returns:
        ``{"success": True}`` on success, or
        ``{"success": False, "error": "..."}`` on failure.
    """
    resp = await client.set_default_profile(profile_id)
    if not resp.success:
        return {
            "success": False,
            "error": resp.error or "Failed to set default profile",
        }
    return {"success": True}
