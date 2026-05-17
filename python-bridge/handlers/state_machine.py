"""State-machine WS bridge handlers.

Implements the runner-side response to commands routed through the
qontinui-web ``runner_command_ws`` relay. Each public coroutine takes a
JSON-serialisable payload dict (already-validated wire envelope) and
returns a JSON-serialisable response dict.

Wire contract:
- Inbound payload validates against
  :class:`qontinui_schemas.commands.state_machine.DiscoverUIBridgeRequest`.
- On success: returns
  :class:`qontinui_schemas.commands.state_machine.DiscoverUIBridgeResponse`
  ``.model_dump(mode='json')``.
- On qontinui-side exception: returns
  :class:`qontinui_schemas.commands.state_machine.DiscoverUIBridgeError`
  ``.model_dump(mode='json')`` (the dispatcher logs + re-raises if needed).

Phase 2 of plan
``plans/2026-05-17-web-runner-ws-bridge-plan-b.md`` introduces this
module + the ``state_machine.discover_ui_bridge`` command. Phases 3-7
extend it with additional commands.
"""

from __future__ import annotations

import logging
import traceback
from typing import Any

from qontinui_schemas.commands.state_machine import (
    DiscoverUIBridgeError,
    DiscoverUIBridgeRequest,
    DiscoverUIBridgeResponse,
)

logger = logging.getLogger(__name__)


def _state_to_dict(state: Any) -> dict[str, Any]:
    """Serialize a ``DiscoveredState`` dataclass to a dict.

    The result uses the dataclass attribute names (``element_ids``,
    ``render_ids``, etc.) so the qontinui-web pydantic models — which
    declare those names as aliases via ``populate_by_name=True`` — can
    consume the payload directly.
    """
    return {
        "id": state.id,
        "name": state.name,
        "element_ids": list(state.element_ids),
        "render_ids": list(state.render_ids),
        "confidence": state.confidence,
        "position_zone": state.position_zone,
        "landmark_context": state.landmark_context,
        "is_global": state.is_global,
        "is_modal": state.is_modal,
    }


def _element_to_dict(element: Any) -> dict[str, Any]:
    """Serialize a ``DiscoveredElement`` dataclass to a dict."""
    return {
        "id": element.id,
        "name": element.name,
        "element_type": element.element_type,
        "render_ids": list(element.render_ids),
        "fingerprint_hash": element.fingerprint_hash,
        "position_zone": element.position_zone,
    }


async def discover_ui_bridge(payload: dict[str, Any]) -> dict[str, Any]:
    """Handle ``state_machine.discover_ui_bridge`` WS bridge command.

    Runs :class:`qontinui.discovery.state_discovery.StateDiscoveryService`
    against the supplied render-log entries (or a co-occurrence export
    if provided) and returns the discovered states + elements.

    Args:
        payload: Inbound JSON dict (wire envelope; validates against
            :class:`DiscoverUIBridgeRequest`).

    Returns:
        JSON dict; either :class:`DiscoverUIBridgeResponse`
        ``.model_dump(mode='json')`` on success or
        :class:`DiscoverUIBridgeError` ``.model_dump(mode='json')`` on
        runner-side failure.
    """
    try:
        req = DiscoverUIBridgeRequest.model_validate(payload)
    except Exception as exc:  # noqa: BLE001 - intentionally broad
        logger.exception("discover_ui_bridge: payload validation failed")
        return DiscoverUIBridgeError(
            request_id=_safe_request_id(payload),
            error="invalid_payload",
            message=f"payload validation failed: {exc!s}",
            traceback=traceback.format_exc(),
        ).model_dump(mode="json")

    try:
        # Lazy-import qontinui so importing this module is cheap when
        # the command is never invoked.
        from qontinui.discovery.state_discovery import (
            DiscoveryStrategyType,
            StateDiscoveryService,
        )

        strategy_map = {
            "auto": DiscoveryStrategyType.AUTO,
            "fingerprint": DiscoveryStrategyType.FINGERPRINT,
        }
        strategy = strategy_map.get(req.strategy, DiscoveryStrategyType.AUTO)

        service = StateDiscoveryService()
        if req.cooccurrence_export is not None:
            result = service.discover_from_export(
                req.cooccurrence_export,
                strategy=strategy,
            )
        else:
            result = service.discover_from_renders(
                req.renders,
                include_html_ids=req.include_html_ids,
                strategy=strategy,
            )

        response = DiscoverUIBridgeResponse(
            request_id=req.request_id,
            states=[_state_to_dict(s) for s in result.states],
            elements=[_element_to_dict(e) for e in result.elements],
            element_to_renders={
                k: list(v) for k, v in result.element_to_renders.items()
            },
            render_count=result.render_count,
            unique_element_count=result.unique_element_count,
            strategy_used=result.strategy_used.value,
            strategy_metadata=result.strategy_metadata or {},
        )
        return response.model_dump(mode="json")

    except Exception as exc:  # noqa: BLE001 - intentionally broad
        logger.exception("discover_ui_bridge: runner-side execution failed")
        return DiscoverUIBridgeError(
            request_id=req.request_id,
            error="qontinui_exception",
            message=str(exc),
            traceback=traceback.format_exc(),
        ).model_dump(mode="json")


_ZERO_UUID = "00000000-0000-0000-0000-000000000000"


def _safe_request_id(payload: dict[str, Any]) -> str:
    """Best-effort recovery of a UUID-shaped request_id from a payload.

    Returns the all-zero UUID when the payload's ``request_id`` is
    missing or not UUID-shaped. Used by error envelopes so that an
    invalid-payload response still validates against the response
    schema.
    """
    from uuid import UUID

    raw = payload.get("request_id")
    if not isinstance(raw, str):
        return _ZERO_UUID
    try:
        return str(UUID(raw))
    except (ValueError, AttributeError):
        return _ZERO_UUID


__all__ = [
    "discover_ui_bridge",
]
