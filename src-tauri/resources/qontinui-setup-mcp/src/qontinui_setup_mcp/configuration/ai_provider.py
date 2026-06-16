"""AI provider configuration via the runner API.

Wraps the runner's AI settings endpoints with a convenient interface
for reading, updating, and testing provider configurations.
"""

from __future__ import annotations

import logging
from typing import Any

from qontinui_setup_mcp.client import RunnerClient

logger = logging.getLogger(__name__)

# Provider keys that correspond to CLI-based providers.
_CLI_PROVIDERS: frozenset[str] = frozenset({"claude_cli", "gemini_cli"})

# Provider keys that correspond to API-based providers.
_API_PROVIDERS: frozenset[str] = frozenset({"claude_api", "gemini_api"})

# All recognized provider keys.
_VALID_PROVIDERS: frozenset[str] = _CLI_PROVIDERS | _API_PROVIDERS


async def get_ai_settings(client: RunnerClient) -> dict[str, Any]:
    """Get current AI provider settings.

    Returns the full settings object with any sensitive data stripped
    (API keys are never present in the settings response from the runner,
    but this function serves as a safety boundary).
    """
    resp = await client.get_ai_settings()
    if not resp.success:
        return {"success": False, "error": resp.error or "Failed to fetch AI settings"}

    settings: dict[str, Any] = resp.data if isinstance(resp.data, dict) else {}

    # Defensive strip: remove any keys that look like secrets.
    _strip_sensitive(settings)

    return {"success": True, **settings}


async def set_ai_provider(
    client: RunnerClient,
    provider: str,
    model: str | None = None,
    cli_execution_mode: str | None = None,
) -> dict[str, Any]:
    """Configure the AI provider via read-modify-write.

    Args:
        client: The runner HTTP client.
        provider: One of ``"claude_cli"``, ``"claude_api"``, ``"gemini_cli"``,
            or ``"gemini_api"``.
        model: Optional model identifier.  When provided, the model field
            inside the provider's sub-key is updated (e.g.
            ``settings["claude_api"]["model"]``).
        cli_execution_mode: Optional execution mode for CLI providers
            (e.g. ``"auto"``, ``"windows_native"``, ``"wsl"``, ``"native"``).
            Ignored if *provider* is not a CLI provider.

    Returns:
        ``{"success": True}`` on success, or
        ``{"success": False, "error": "..."}`` on failure.
    """
    if provider not in _VALID_PROVIDERS:
        return {
            "success": False,
            "error": (
                f"Invalid provider '{provider}'. "
                f"Must be one of: {', '.join(sorted(_VALID_PROVIDERS))}"
            ),
        }

    # Read current settings.
    resp = await client.get_ai_settings()
    if not resp.success:
        return {"success": False, "error": resp.error or "Failed to read AI settings"}

    settings: dict[str, Any] = resp.data if isinstance(resp.data, dict) else {}

    # Set the active provider.
    settings["provider"] = provider

    # Ensure the provider sub-key exists.
    if provider not in settings or not isinstance(settings[provider], dict):
        settings[provider] = {}

    # Update model if provided.
    if model is not None:
        settings[provider]["model"] = model

    # Update CLI execution mode if applicable.
    if cli_execution_mode is not None and provider in _CLI_PROVIDERS:
        settings[provider]["execution_mode"] = cli_execution_mode

    put_resp = await client.put_ai_settings(settings)
    if not put_resp.success:
        return {
            "success": False,
            "error": put_resp.error or "Failed to save AI settings",
        }

    return {"success": True}


async def store_api_key(
    client: RunnerClient,
    provider: str,
    api_key: str,
) -> dict[str, Any]:
    """Store an API key in the OS keychain via the runner.

    Args:
        client: The runner HTTP client.
        provider: The provider name (e.g. ``"claude_api"``, ``"gemini_api"``).
        api_key: The API key value.

    Returns:
        ``{"success": True}`` on success, or
        ``{"success": False, "error": "..."}`` on failure.
    """
    resp = await client.store_api_key(provider, api_key)
    if not resp.success:
        return {"success": False, "error": resp.error or "Failed to store API key"}
    return {"success": True}


async def check_api_key(client: RunnerClient, provider: str) -> dict[str, Any]:
    """Check whether an API key exists for a given provider.

    Returns:
        ``{"success": True, "has_key": bool}`` on success, or
        ``{"success": False, "error": "..."}`` on failure.
    """
    resp = await client.check_api_key(provider)
    if not resp.success:
        return {"success": False, "error": resp.error or "Failed to check API key"}

    has_key: bool = False
    if isinstance(resp.data, dict):
        has_key = bool(resp.data.get("has_key", False))
    elif isinstance(resp.data, bool):
        has_key = resp.data

    return {"success": True, "has_key": has_key}


async def test_ai_connection(client: RunnerClient) -> dict[str, Any]:
    """Test AI connectivity using the currently configured provider.

    Returns:
        ``{"success": True, ...}`` with connection test details on success,
        or ``{"success": False, "error": "..."}`` on failure.
    """
    resp = await client.test_ai_connection()
    if not resp.success:
        return {"success": False, "error": resp.error or "AI connection test failed"}

    result: dict[str, Any] = {"success": True}
    if isinstance(resp.data, dict):
        result.update(resp.data)

    return result


# ── Helpers ──────────────────────────────────────────────────────────────


def _strip_sensitive(settings: dict[str, Any]) -> None:
    """Remove any keys that look like they might contain secrets (in-place).

    The runner API should never expose raw keys, but this provides a
    defense-in-depth layer.
    """
    sensitive_keys = {"api_key", "secret", "token", "password", "credential"}
    keys_to_remove: list[str] = []

    for key in settings:
        if any(s in key.lower() for s in sensitive_keys):
            keys_to_remove.append(key)

    for key in keys_to_remove:
        del settings[key]

    # Recurse into nested dicts.
    for value in settings.values():
        if isinstance(value, dict):
            _strip_sensitive(value)
