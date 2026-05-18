"""Click-analysis WS bridge handlers.

Implements the runner-side response to ``click_analysis.tune_profile``
commands routed through the qontinui-web ``runner_command_ws`` relay.
The handler downloads the supplied screenshot URLs, runs
:class:`qontinui.discovery.click_analysis.ApplicationTuner` against the
decoded screenshots + ``known_elements`` ground-truth boxes, and returns
the serialised ``TuningResult`` payload.

Wire contract: handler accepts a JSON payload (already-validated wire
envelope) and returns a JSON-serialisable response dict — either a
successful :class:`TuneProfileResponse` or an error envelope
(:class:`TuneProfileError` with ``invalid_payload`` /
``screenshot_download_failed`` / ``qontinui_exception`` /
``internal_error``). The Rust-side dispatcher
(``mcp/ws_bridge_dispatch.rs``) wraps the response in a
``command_response`` WS frame; web-side ``dispatch_and_wait`` correlates
by ``request_id``.

Phase 5 of plan
``plans/2026-05-17-web-runner-ws-bridge-plan-b.md`` introduces this
module + the single ``click_analysis.tune_profile`` command.
"""

from __future__ import annotations

import logging
import traceback
from typing import Any

import httpx
import numpy as np

try:
    import cv2
except ImportError:  # pragma: no cover - cv2 always available in runner venv
    cv2 = None  # type: ignore[assignment]

from qontinui_schemas.commands.click_analysis import (
    TuneProfileError,
    TuneProfileRequest,
    TuneProfileResponse,
)

logger = logging.getLogger(__name__)


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


def _download_screenshot(url: str, *, timeout_s: float = 20.0) -> np.ndarray:
    """Download a screenshot URL and decode it to a BGR numpy array.

    Args:
        url: HTTP(S) URL to fetch (typically a presigned S3 URL).
        timeout_s: Per-request timeout in seconds.

    Returns:
        OpenCV BGR ``ndarray`` of shape ``(H, W, 3)``.

    Raises:
        RuntimeError: If the HTTP fetch fails, the response is empty,
            or cv2 cannot decode the bytes as an image.
    """
    if cv2 is None:
        raise RuntimeError(
            "cv2 (opencv-python) is required by ApplicationTuner but is not "
            "installed in the runner-Python venv"
        )

    try:
        response = httpx.get(url, timeout=timeout_s, follow_redirects=True)
        response.raise_for_status()
    except httpx.HTTPError as exc:
        raise RuntimeError(f"screenshot fetch failed: {url} - {exc!s}") from exc

    if not response.content:
        raise RuntimeError(f"screenshot fetch returned empty body: {url}")

    buf = np.frombuffer(response.content, dtype=np.uint8)
    img = cv2.imdecode(buf, cv2.IMREAD_COLOR)
    if img is None:
        raise RuntimeError(
            f"screenshot decode failed (unsupported format?): {url}"
        )
    return img


def _build_known_element(item: dict[str, Any]) -> Any:
    """Reconstruct an ``InferredBoundingBox`` from a JSON dict.

    Tolerates the wire-shape from
    :class:`qontinui_schemas.template_capture.CandidateBoundingBox`
    (``strategy_used`` / ``element_type`` carried as enum-value strings)
    and the qontinui-side dataclass shape directly.
    """
    from qontinui.discovery.click_analysis.models import (
        DetectionStrategy,
        ElementType,
        InferredBoundingBox,
    )

    raw_strategy = item.get("strategy_used", "fixed_size")
    if isinstance(raw_strategy, DetectionStrategy):
        strategy = raw_strategy
    else:
        try:
            strategy = DetectionStrategy(raw_strategy)
        except ValueError:
            strategy = DetectionStrategy.FIXED_SIZE

    raw_elem_type = item.get("element_type", "unknown")
    if isinstance(raw_elem_type, ElementType):
        element_type = raw_elem_type
    else:
        try:
            element_type = ElementType(raw_elem_type)
        except ValueError:
            element_type = ElementType.UNKNOWN

    return InferredBoundingBox(
        x=int(item["x"]),
        y=int(item["y"]),
        width=int(item["width"]),
        height=int(item["height"]),
        confidence=float(item.get("confidence", 0.0)),
        strategy_used=strategy,
        element_type=element_type,
        metadata=dict(item.get("metadata") or {}),
    )


async def tune_profile(payload: dict[str, Any]) -> dict[str, Any]:
    """Handle ``click_analysis.tune_profile`` WS bridge command.

    Downloads the supplied screenshot URLs, runs
    :class:`qontinui.discovery.click_analysis.ApplicationTuner` against
    the decoded images + ``known_elements`` ground-truth boxes, and
    returns the serialised ``TuningResult`` payload.

    Args:
        payload: Inbound JSON dict (wire envelope; validates against
            :class:`TuneProfileRequest`).

    Returns:
        JSON dict; either :class:`TuneProfileResponse`
        ``.model_dump(mode='json')`` on success or
        :class:`TuneProfileError` ``.model_dump(mode='json')`` on
        runner-side failure.
    """
    try:
        req = TuneProfileRequest.model_validate(payload)
    except Exception as exc:  # noqa: BLE001 - intentionally broad
        logger.exception("tune_profile: payload validation failed")
        return TuneProfileError(
            request_id=_safe_request_id(payload),
            error="invalid_payload",
            message=f"payload validation failed: {exc!s}",
            traceback=traceback.format_exc(),
        ).model_dump(mode="json")

    # Download screenshots in URL order.
    screenshots: list[np.ndarray] = []
    try:
        for url in req.screenshot_urls:
            screenshots.append(_download_screenshot(url))
    except RuntimeError as exc:
        logger.exception("tune_profile: screenshot download failed")
        return TuneProfileError(
            request_id=req.request_id,
            error="screenshot_download_failed",
            message=str(exc),
            traceback=traceback.format_exc(),
        ).model_dump(mode="json")
    except Exception as exc:  # noqa: BLE001 - intentionally broad
        logger.exception("tune_profile: unexpected error during screenshot fetch")
        return TuneProfileError(
            request_id=req.request_id,
            error="internal_error",
            message=f"screenshot fetch unexpected error: {exc!s}",
            traceback=traceback.format_exc(),
        ).model_dump(mode="json")

    try:
        # Lazy-import qontinui so importing this module is cheap when
        # the command is never invoked.
        from qontinui.discovery.click_analysis import (
            ApplicationTuner,
            InferenceConfig,
        )

        # Optional detection-strategies override: translate the wire
        # literal back into qontinui's DetectionStrategy enum + preseed
        # the tuner's base config with the caller's preferred set.
        base_config: InferenceConfig | None = None
        if req.detection_strategies:
            from qontinui.discovery.click_analysis.models import DetectionStrategy

            preferred = []
            for s in req.detection_strategies:
                try:
                    preferred.append(DetectionStrategy(s))
                except ValueError:
                    logger.warning(
                        "tune_profile: unknown detection_strategy %r — skipped", s
                    )
            if preferred:
                base_config = InferenceConfig(preferred_strategies=preferred)

        # target_element_types is currently advisory only — the tuner
        # API does not consume it yet. Logged for observability.
        if req.target_element_types:
            logger.info(
                "tune_profile: target_element_types hint=%s (advisory only)",
                req.target_element_types,
            )

        known = [_build_known_element(e) for e in req.known_elements]

        tuner = ApplicationTuner(base_config=base_config)
        result = tuner.tune_from_samples(
            screenshots=screenshots,
            known_elements=known or None,
        )

        return TuneProfileResponse(
            request_id=req.request_id,
            tuning_result=result.to_dict(),
        ).model_dump(mode="json")

    except Exception as exc:  # noqa: BLE001 - intentionally broad
        logger.exception("tune_profile: runner-side execution failed")
        return TuneProfileError(
            request_id=req.request_id,
            error="qontinui_exception",
            message=str(exc),
            traceback=traceback.format_exc(),
        ).model_dump(mode="json")


__all__ = ["tune_profile"]
