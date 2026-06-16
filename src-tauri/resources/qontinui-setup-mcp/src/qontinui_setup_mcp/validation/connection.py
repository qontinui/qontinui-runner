"""Test runner connectivity and get full setup status."""

from __future__ import annotations

import logging
from typing import Any

from qontinui_setup_mcp.client import RunnerClient

from .log_validator import validate_log_sources
from .prerequisites import check_prerequisites

logger = logging.getLogger(__name__)

# Points allocated to each setup check (must sum to 100).
_POINTS = {
    "runner_connectivity": 20,
    "ai_provider_configured": 20,
    "api_key_stored": 15,
    "log_sources_configured": 20,
    "log_source_valid": 15,
    "prerequisites": 10,
}

# Core prerequisite tools that should be present for development.
_CORE_PREREQUISITES = ["node", "python", "git"]


async def check_runner_connection(client: RunnerClient) -> dict[str, Any]:
    """Test runner HTTP API connectivity.

    Calls the runner health endpoint and, on success, also fetches device
    information.

    Returns:
        A dict with *connected*, *host*, *port*, *error*, and *device_info*.
    """
    result: dict[str, Any] = {
        "connected": False,
        "host": client.host,
        "port": client.port,
        "error": None,
        "device_info": None,
    }

    health = await client.health()
    if not health.success:
        result["error"] = health.error or "Runner health check failed"
        return result

    result["connected"] = True

    # Opportunistically fetch device info
    device = await client.get_device_info()
    if device.success and device.data:
        result["device_info"] = device.data

    return result


async def get_setup_status(client: RunnerClient) -> dict[str, Any]:
    """Get a full setup overview with completion percentage and recommendations.

    Checks (each worth points toward completion %):
        1. Runner connectivity (20 pts)
        2. AI provider configured (20 pts)
        3. API key stored (15 pts)
        4. Log sources configured (20 pts)
        5. At least one log source valid (15 pts)
        6. Prerequisites — node, python, git (10 pts)

    Returns a dict with *completion_percentage*, *runner_connected*, *checks*,
    *recommendations*, *ai_provider*, *log_source_count*, and *device_info*.
    """
    checks: list[dict[str, Any]] = []
    recommendations: list[str] = []
    earned_points = 0
    runner_connected = False
    ai_provider: str | None = None
    log_source_count = 0
    device_info: dict[str, Any] | None = None

    # ── 1. Runner connectivity ──────────────────────────────────────────
    conn = await check_runner_connection(client)
    runner_connected = conn["connected"]
    device_info = conn.get("device_info")

    if runner_connected:
        earned_points += _POINTS["runner_connectivity"]
        checks.append(
            {
                "name": "Runner connectivity",
                "status": "pass",
                "detail": f"Connected to runner at {client.host}:{client.port}",
                "points": _POINTS["runner_connectivity"],
            }
        )
    else:
        checks.append(
            {
                "name": "Runner connectivity",
                "status": "fail",
                "detail": conn.get("error", "Cannot reach runner"),
                "points": 0,
            }
        )
        recommendations.append(
            "Start the qontinui-runner (dev-start.ps1 -Runner) and try again."
        )

    # ── 2 & 3. AI provider + API key ───────────────────────────────────
    if runner_connected:
        ai_resp = await client.get_ai_settings()
        if ai_resp.success and ai_resp.data:
            provider = (
                ai_resp.data.get("provider")
                or ai_resp.data.get("ai_provider")
                or ai_resp.data.get("default_provider")
            )
            if provider:
                ai_provider = provider
                earned_points += _POINTS["ai_provider_configured"]
                checks.append(
                    {
                        "name": "AI provider configured",
                        "status": "pass",
                        "detail": f"Provider set to '{provider}'",
                        "points": _POINTS["ai_provider_configured"],
                    }
                )

                # Check API key for the configured provider
                key_resp = await client.check_api_key(provider)
                if key_resp.success and key_resp.data:
                    has_key = (
                        key_resp.data.get("has_key", False)
                        if isinstance(key_resp.data, dict)
                        else bool(key_resp.data)
                    )
                else:
                    has_key = False

                if has_key:
                    earned_points += _POINTS["api_key_stored"]
                    checks.append(
                        {
                            "name": "API key stored",
                            "status": "pass",
                            "detail": f"API key present for '{provider}'",
                            "points": _POINTS["api_key_stored"],
                        }
                    )
                else:
                    checks.append(
                        {
                            "name": "API key stored",
                            "status": "fail",
                            "detail": f"No API key found for '{provider}'",
                            "points": 0,
                        }
                    )
                    recommendations.append(
                        f"Store an API key for the '{provider}' provider."
                    )
            else:
                checks.append(
                    {
                        "name": "AI provider configured",
                        "status": "fail",
                        "detail": "No AI provider selected",
                        "points": 0,
                    }
                )
                checks.append(
                    {
                        "name": "API key stored",
                        "status": "fail",
                        "detail": "Cannot check key — no provider configured",
                        "points": 0,
                    }
                )
                recommendations.append(
                    "Configure an AI provider in the runner settings."
                )
        else:
            checks.append(
                {
                    "name": "AI provider configured",
                    "status": "fail",
                    "detail": ai_resp.error or "Could not retrieve AI settings",
                    "points": 0,
                }
            )
            checks.append(
                {
                    "name": "API key stored",
                    "status": "fail",
                    "detail": "Cannot check key — AI settings unavailable",
                    "points": 0,
                }
            )
            recommendations.append("Configure AI settings in the runner.")
    else:
        # Runner offline — skip AI checks
        checks.append(
            {
                "name": "AI provider configured",
                "status": "fail",
                "detail": "Skipped — runner not connected",
                "points": 0,
            }
        )
        checks.append(
            {
                "name": "API key stored",
                "status": "fail",
                "detail": "Skipped — runner not connected",
                "points": 0,
            }
        )

    # ── 4 & 5. Log sources ─────────────────────────────────────────────
    if runner_connected:
        log_validation = await validate_log_sources(client, check_freshness=True)
        log_source_count = log_validation["summary"]["total"]
        valid_count = log_validation["summary"]["valid"]

        if log_source_count > 0:
            earned_points += _POINTS["log_sources_configured"]
            checks.append(
                {
                    "name": "Log sources configured",
                    "status": "pass",
                    "detail": f"{log_source_count} log source(s) configured",
                    "points": _POINTS["log_sources_configured"],
                }
            )

            if valid_count > 0:
                earned_points += _POINTS["log_source_valid"]
                checks.append(
                    {
                        "name": "Log source valid",
                        "status": "pass",
                        "detail": f"{valid_count}/{log_source_count} source(s) valid",
                        "points": _POINTS["log_source_valid"],
                    }
                )
            else:
                checks.append(
                    {
                        "name": "Log source valid",
                        "status": "warn",
                        "detail": "No log sources passed validation",
                        "points": 0,
                    }
                )
                recommendations.append(
                    "Check that log source paths exist and are readable."
                )
        else:
            checks.append(
                {
                    "name": "Log sources configured",
                    "status": "fail",
                    "detail": "No log sources configured",
                    "points": 0,
                }
            )
            checks.append(
                {
                    "name": "Log source valid",
                    "status": "fail",
                    "detail": "No log sources to validate",
                    "points": 0,
                }
            )
            recommendations.append(
                "Add at least one log source in the runner settings."
            )
    else:
        checks.append(
            {
                "name": "Log sources configured",
                "status": "fail",
                "detail": "Skipped — runner not connected",
                "points": 0,
            }
        )
        checks.append(
            {
                "name": "Log source valid",
                "status": "fail",
                "detail": "Skipped — runner not connected",
                "points": 0,
            }
        )

    # ── 6. Prerequisites ────────────────────────────────────────────────
    prereq_result = await check_prerequisites(checks=_CORE_PREREQUISITES)
    prereq_summary = prereq_result["summary"]

    if prereq_summary["missing"] == 0:
        earned_points += _POINTS["prerequisites"]
        checks.append(
            {
                "name": "Core prerequisites",
                "status": "pass",
                "detail": (
                    f"All {prereq_summary['total']} core tools installed "
                    f"({', '.join(_CORE_PREREQUISITES)})"
                ),
                "points": _POINTS["prerequisites"],
            }
        )
    else:
        missing_tools = [
            r["tool"] for r in prereq_result["results"] if not r["installed"]
        ]
        if prereq_summary["installed"] > 0:
            status = "warn"
            earned_points += _POINTS["prerequisites"] // 2
            points_awarded = _POINTS["prerequisites"] // 2
        else:
            status = "fail"
            points_awarded = 0

        checks.append(
            {
                "name": "Core prerequisites",
                "status": status,
                "detail": f"Missing: {', '.join(missing_tools)}",
                "points": points_awarded,
            }
        )
        recommendations.append(f"Install missing tools: {', '.join(missing_tools)}.")

    # ── Final summary ───────────────────────────────────────────────────
    total_possible = sum(_POINTS.values())
    completion_percentage = int(round(earned_points / total_possible * 100))

    return {
        "completion_percentage": completion_percentage,
        "runner_connected": runner_connected,
        "checks": checks,
        "recommendations": recommendations,
        "ai_provider": ai_provider,
        "log_source_count": log_source_count,
        "device_info": device_info,
    }
