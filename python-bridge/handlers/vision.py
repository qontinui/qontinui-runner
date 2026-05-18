"""Vision WS bridge handlers.

Implements the runner-side response to the ``vision.compute_perceptual_hash``
and ``vision.compare_screenshots`` commands routed through the
qontinui-web ``runner_command_ws`` relay. Each public coroutine takes a
JSON-serialisable payload dict (already-validated wire envelope) and
returns a JSON-serialisable response dict.

Wire contract: each handler accepts a JSON payload (already-validated
wire envelope) and returns a JSON-serialisable response dict — either a
successful ``…Response`` or an error envelope
(``invalid_payload`` / ``qontinui_exception`` / etc.). The Rust-side
dispatcher (``mcp/backend_relay.rs``) wraps the response in a
``command_response`` WS frame; web-side ``dispatch_and_wait`` correlates
by ``request_id``.

Image bytes flow as base64 (PNG/JPEG) in both directions:

- Inbound: ``image_b64`` (for hash) or ``baseline_b64`` + ``current_b64``
  (for compare). Decoded to RGB numpy arrays via PIL.
- Outbound (compare only, on failed comparisons): ``diff_image_b64``
  carrying the diff PNG produced by
  ``VisualComparator.generate_diff_image``.

Phase 7 of plan ``plans/2026-05-17-web-runner-ws-bridge-plan-b.md``.
"""

from __future__ import annotations

import base64
import io
import logging
import traceback
from typing import Any

from qontinui_schemas.commands.vision import (
    CompareScreenshotsError,
    CompareScreenshotsRequest,
    CompareScreenshotsResponse,
    CompareScreenshotsResult,
    ComputePerceptualHashError,
    ComputePerceptualHashRequest,
    ComputePerceptualHashResponse,
)

logger = logging.getLogger(__name__)

_ZERO_UUID = "00000000-0000-0000-0000-000000000000"


def _safe_request_id(payload: dict[str, Any]) -> str:
    """Best-effort recovery of a UUID-shaped ``request_id`` from a payload.

    Returns the all-zero UUID when the payload's ``request_id`` is
    missing or not UUID-shaped. Used by error envelopes so an
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


def _b64_to_rgb_numpy(image_b64: str) -> Any:
    """Decode a base64-encoded image to an RGB numpy array.

    Accepts any format Pillow can decode (PNG/JPEG/...). The result is
    a contiguous 3-channel uint8 array in RGB byte order (NOT BGR);
    :class:`VisualComparator` accepts either as long as it's consistent
    across baseline + current.
    """
    import numpy as np
    from PIL import Image

    raw = base64.b64decode(image_b64, validate=True)
    with Image.open(io.BytesIO(raw)) as img:
        return np.array(img.convert("RGB"))


async def compute_perceptual_hash(payload: dict[str, Any]) -> dict[str, Any]:
    """Handle ``vision.compute_perceptual_hash`` WS bridge command.

    Runs :meth:`qontinui.vision.comparison.VisualComparator.compute_perceptual_hash`
    against a single image and returns its hex repr.

    Args:
        payload: Inbound JSON dict (wire envelope; validates against
            :class:`ComputePerceptualHashRequest`).

    Returns:
        JSON dict; either :class:`ComputePerceptualHashResponse`
        ``.model_dump(mode='json')`` on success or
        :class:`ComputePerceptualHashError`
        ``.model_dump(mode='json')`` on runner-side failure.
    """
    try:
        req = ComputePerceptualHashRequest.model_validate(payload)
    except Exception as exc:  # noqa: BLE001 - intentionally broad
        logger.exception("compute_perceptual_hash: payload validation failed")
        return ComputePerceptualHashError(
            request_id=_safe_request_id(payload),
            error="invalid_payload",
            message=f"payload validation failed: {exc!s}",
            traceback=traceback.format_exc(),
        ).model_dump(mode="json")

    try:
        # Lazy-import qontinui so importing this module is cheap when
        # the command is never invoked.
        from qontinui.vision.comparison import VisualComparator

        image = _b64_to_rgb_numpy(req.image_b64)
        comparator = VisualComparator()
        try:
            hash_hex = comparator.compute_perceptual_hash(
                image, hash_size=req.hash_size
            )
        except RuntimeError as runtime_exc:
            # VisualComparator.compute_perceptual_hash raises RuntimeError
            # when the optional `imagehash` package is missing.
            msg = str(runtime_exc)
            if "imagehash" in msg.lower():
                logger.exception(
                    "compute_perceptual_hash: imagehash extension missing"
                )
                return ComputePerceptualHashError(
                    request_id=req.request_id,
                    error="imagehash_unavailable",
                    message=msg,
                    traceback=traceback.format_exc(),
                ).model_dump(mode="json")
            raise

        response = ComputePerceptualHashResponse(
            request_id=req.request_id,
            hash_hex=str(hash_hex),
        )
        return response.model_dump(mode="json")

    except Exception as exc:  # noqa: BLE001 - intentionally broad
        logger.exception("compute_perceptual_hash: runner-side execution failed")
        return ComputePerceptualHashError(
            request_id=req.request_id,
            error="qontinui_exception",
            message=str(exc),
            traceback=traceback.format_exc(),
        ).model_dump(mode="json")


async def compare_screenshots(payload: dict[str, Any]) -> dict[str, Any]:
    """Handle ``vision.compare_screenshots`` WS bridge command.

    Runs :meth:`qontinui.vision.comparison.VisualComparator.compare`
    over the supplied baseline + current images and returns the
    standard :class:`ComparisonResult`-shaped payload. When
    ``generate_diff_image=True`` AND the comparison fails AND a diff
    mask is producible, also generates a diff PNG (base64-encoded) and
    returns it in ``diff_image_b64`` so the web side can upload it via
    its existing ``_upload_diff_image`` helper.

    Args:
        payload: Inbound JSON dict (wire envelope; validates against
            :class:`CompareScreenshotsRequest`).

    Returns:
        JSON dict; either :class:`CompareScreenshotsResponse`
        ``.model_dump(mode='json')`` on success or
        :class:`CompareScreenshotsError`
        ``.model_dump(mode='json')`` on runner-side failure.
    """
    try:
        req = CompareScreenshotsRequest.model_validate(payload)
    except Exception as exc:  # noqa: BLE001 - intentionally broad
        logger.exception("compare_screenshots: payload validation failed")
        return CompareScreenshotsError(
            request_id=_safe_request_id(payload),
            error="invalid_payload",
            message=f"payload validation failed: {exc!s}",
            traceback=traceback.format_exc(),
        ).model_dump(mode="json")

    try:
        from qontinui.vision.comparison import (
            ComparisonAlgorithm,
            IgnoreRegion,
            VisualComparator,
        )

        baseline_img = _b64_to_rgb_numpy(req.baseline_b64)
        current_img = _b64_to_rgb_numpy(req.current_b64)

        ignore_regions = [
            IgnoreRegion.from_dict(region.model_dump(mode="python"))
            for region in req.ignore_regions
        ]

        # Map our wire-literal to the qontinui ComparisonAlgorithm enum.
        # The enum's StrEnum membership accepts the string values
        # directly, so we round-trip through ComparisonAlgorithm() to
        # validate the literal up-front (cheaper failure mode than a
        # mid-compute KeyError) and pass the enum into compare().
        algorithm_enum = ComparisonAlgorithm(req.algorithm)

        comparator = VisualComparator()
        comp_result = comparator.compare(
            baseline=baseline_img,
            screenshot=current_img,
            algorithm=algorithm_enum,
            threshold=req.threshold,
            ignore_regions=ignore_regions,
        )

        result_payload = CompareScreenshotsResult(
            similarity_score=float(comp_result.similarity_score),
            passed=bool(comp_result.passed),
            threshold=float(comp_result.threshold),
            algorithm=comp_result.algorithm.value,
            execution_time_ms=int(comp_result.execution_time_ms),
            diff_regions=[r.to_dict() for r in comp_result.diff_regions],
            error=comp_result.error,
        )

        diff_image_b64: str | None = None
        if (
            req.generate_diff_image
            and not comp_result.passed
            and comp_result.diff_mask is not None
        ):
            try:
                diff_png = comparator.generate_diff_image(
                    baseline_img, current_img, comp_result.diff_mask
                )
                diff_image_b64 = base64.b64encode(diff_png).decode("ascii")
            except Exception:  # noqa: BLE001 - best effort
                # Diff-image generation is non-critical (the comparison
                # result already carries the diff_regions metadata). Log
                # and continue; the web side will surface the
                # comparison result without a diff PNG attachment.
                logger.exception(
                    "compare_screenshots: diff-image generation failed"
                )

        response = CompareScreenshotsResponse(
            request_id=req.request_id,
            result=result_payload,
            diff_image_b64=diff_image_b64,
        )
        return response.model_dump(mode="json")

    except Exception as exc:  # noqa: BLE001 - intentionally broad
        logger.exception("compare_screenshots: runner-side execution failed")
        return CompareScreenshotsError(
            request_id=req.request_id,
            error="qontinui_exception",
            message=str(exc),
            traceback=traceback.format_exc(),
        ).model_dump(mode="json")


__all__ = [
    "compute_perceptual_hash",
    "compare_screenshots",
]
