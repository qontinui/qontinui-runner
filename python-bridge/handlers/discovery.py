"""Discovery-pipeline WS bridge handlers.

Implements the runner-side response to commands routed through the
qontinui-web ``runner_command_ws`` relay for discovery-pipeline work
that historically lived directly on the web backend but now runs
runner-side via the existing WS dispatcher.

Wire contract: each handler accepts a JSON payload (already-validated
wire envelope) and returns a JSON-serialisable response dict — either
a successful ``…Response`` or an error envelope
(``invalid_payload`` / ``qontinui_exception`` / etc.). The Rust-side
dispatcher (``mcp/ws_bridge_dispatch.rs``) wraps the response in a
``command_response`` WS frame; web-side ``dispatch_and_wait`` correlates
by ``request_id``.

Phase 6 of plan ``plans/2026-05-17-web-runner-ws-bridge-plan-b.md``
introduces this module with one command:

- ``discovery.background_removal`` — invokes
  :func:`qontinui.discovery.background_removal.remove_backgrounds_from_base64`
  and returns the masked screenshots + analyser statistics.
"""

from __future__ import annotations

import logging
import traceback
from typing import Any

from qontinui_schemas.commands.discovery import (
    BackgroundRemovalError,
    BackgroundRemovalRequest,
    BackgroundRemovalResponse,
)

logger = logging.getLogger(__name__)


_ZERO_UUID = "00000000-0000-0000-0000-000000000000"


def _safe_request_id(payload: dict[str, Any]) -> str:
    """Best-effort recovery of a UUID-shaped ``request_id`` from a payload.

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


async def background_removal(payload: dict[str, Any]) -> dict[str, Any]:
    """Handle ``discovery.background_removal`` WS bridge command.

    Decodes the request, invokes
    :func:`qontinui.discovery.background_removal.remove_backgrounds_from_base64`,
    and returns the masked screenshots plus the analyser statistics
    dict (pixel counts, percentages, image size, optional debug mask).

    Args:
        payload: Inbound JSON dict (wire envelope; validates against
            :class:`BackgroundRemovalRequest`).

    Returns:
        JSON dict; either :class:`BackgroundRemovalResponse`
        ``.model_dump(mode='json')`` on success or
        :class:`BackgroundRemovalError` ``.model_dump(mode='json')`` on
        runner-side failure.
    """
    try:
        req = BackgroundRemovalRequest.model_validate(payload)
    except Exception as exc:  # noqa: BLE001 - intentionally broad
        logger.exception("background_removal: payload validation failed")
        return BackgroundRemovalError(
            request_id=_safe_request_id(payload),  # type: ignore[arg-type]
            error="invalid_payload",
            message=f"payload validation failed: {exc!s}",
            traceback=traceback.format_exc(),
        ).model_dump(mode="json")

    try:
        # Lazy-import qontinui so importing this module is cheap when
        # the command is never invoked.
        from qontinui.discovery.background_removal import (
            BackgroundRemovalConfig,
            remove_backgrounds_from_base64,
        )

        cfg = (
            BackgroundRemovalConfig(**req.config)
            if req.config is not None
            else BackgroundRemovalConfig()
        )

        masked_b64, stats = remove_backgrounds_from_base64(
            req.screenshots_b64,
            config=cfg,
            debug=req.debug,
        )

        # ``stats`` may include a numpy ``image_size`` tuple; pydantic's
        # JSON mode handles tuples natively but downstream consumers
        # expect a list. Coerce to a list for wire-friendliness.
        if isinstance(stats.get("image_size"), tuple):
            stats = {**stats, "image_size": list(stats["image_size"])}

        response = BackgroundRemovalResponse(
            request_id=req.request_id,
            masked_screenshots_b64=masked_b64,
            statistics=stats,
        )
        return response.model_dump(mode="json")

    except Exception as exc:  # noqa: BLE001 - intentionally broad
        logger.exception("background_removal: runner-side execution failed")
        return BackgroundRemovalError(
            request_id=req.request_id,
            error="qontinui_exception",
            message=str(exc),
            traceback=traceback.format_exc(),
        ).model_dump(mode="json")
