"""Trajectory logger for grounding-model fine-tuning data.

Captures ``(pre_screenshot, action, post_screenshot, success_label)`` tuples
during native-app workflow execution and writes them as
:class:`GroundingRecord` entries to ``grounding.jsonl``.

**Default OFF.**  Opt-in via ``QONTINUI_TRAJECTORY_LOGGER=on``.

The logger hooks into the action execution lifecycle at two points:

1. **Pre-action** — ``on_action_start()`` captures a screenshot of the
   current screen state.
2. **Post-action** — ``on_record_created()`` captures a second screenshot,
   computes a success label, optionally enriches elements via OmniParser,
   and writes the grounding record.

Success labelling strategy (in priority order):

- World State Verifier (WSM) if reachable.
- Pixel-diff heuristic: mean pixel change in a 100×100 region around the
  click target.
- Fallback: the action record's own ``success`` flag.
"""

from __future__ import annotations

import io
import logging
import uuid
from datetime import datetime, timezone
from pathlib import Path
from typing import TYPE_CHECKING, Any

import numpy as np
from PIL import Image

logger = logging.getLogger(__name__)

if TYPE_CHECKING:
    from models.action_execution_record import ActionExecutionRecord

# Import grounding schema — lives in qontinui-train which is on sys.path
try:
    from qontinui_train.export.grounding_record import (
        GroundingAction,
        GroundingElement,
        GroundingJSONLWriter,
        GroundingRecord,
    )

    _GROUNDING_AVAILABLE = True
except ImportError:
    _GROUNDING_AVAILABLE = False
    logger.debug("qontinui_train.export.grounding_record not available")

# Pixel-diff threshold: mean absolute difference per channel per pixel.
_PIXEL_DIFF_THRESHOLD = 5.0

# Region half-size (pixels) around click target for pixel-diff heuristic.
_DIFF_REGION_HALF = 50


class TrajectoryLogger:
    """Captures action trajectories and writes grounding JSONL records.

    Parameters
    ----------
    output_dir:
        Directory for ``grounding.jsonl`` and ``images/``.
    max_records_per_session:
        Safety cap to prevent disk fill.  Default 500.
    wsm_client:
        Optional World State Verifier client.  If ``None`` or unreachable,
        the pixel-diff heuristic is used instead.
    omniparser_detector:
        Optional ``OmniParserDetector`` for enriching records with
        interactability labels via the fast YOLO-only path.
    """

    def __init__(
        self,
        output_dir: Path,
        max_records_per_session: int = 500,
        wsm_client: Any | None = None,
        omniparser_detector: Any | None = None,
    ) -> None:
        if not _GROUNDING_AVAILABLE:
            raise ImportError(
                "qontinui_train.export.grounding_record is required for TrajectoryLogger"
            )

        self._writer = GroundingJSONLWriter(Path(output_dir))
        self._max_records = max_records_per_session
        self._record_count = 0
        self._session_id = str(uuid.uuid4())
        self._wsm_client = wsm_client
        self._omniparser = omniparser_detector

        # Pre-action state — set by on_action_start, consumed by on_record_created
        self._pre_png_bytes: bytes | None = None
        self._pre_array: np.ndarray | None = None

        logger.info(
            "TrajectoryLogger initialised: output_dir=%s, max_records=%d, "
            "wsm=%s, omniparser=%s",
            output_dir,
            max_records_per_session,
            "yes" if wsm_client else "no",
            "yes" if omniparser_detector else "no",
        )

    # ------------------------------------------------------------------
    # Public hooks
    # ------------------------------------------------------------------

    def on_action_start(self, action_id: str) -> None:
        """Capture the pre-action screenshot.  Called before action dispatch."""
        if self._record_count >= self._max_records:
            return
        try:
            self._pre_png_bytes, self._pre_array = self._capture_screen()
        except Exception:
            logger.debug("Pre-action screenshot failed", exc_info=True)
            self._pre_png_bytes = None
            self._pre_array = None

    def on_record_created(self, record: ActionExecutionRecord) -> None:
        """Post-action callback matching ``record_created_callback`` signature.

        Captures a post screenshot, computes success label, optionally
        enriches elements via OmniParser, and writes a grounding record.
        """
        if self._record_count >= self._max_records:
            return
        if self._pre_png_bytes is None or self._pre_array is None:
            return

        try:
            post_bytes, post_array = self._capture_screen()
        except Exception:
            logger.debug("Post-action screenshot failed", exc_info=True)
            self._pre_png_bytes = None
            self._pre_array = None
            return

        try:
            self._write_record(record, post_bytes, post_array)
        except Exception:
            logger.warning("TrajectoryLogger failed to write record", exc_info=True)
        finally:
            self._pre_png_bytes = None
            self._pre_array = None

    # ------------------------------------------------------------------
    # Internal
    # ------------------------------------------------------------------

    def _write_record(
        self,
        record: ActionExecutionRecord,
        post_bytes: bytes,
        post_array: np.ndarray,
    ) -> None:
        # Success label
        success, success_source = self._compute_success_label(
            record,
            self._pre_png_bytes,
            self._pre_array,
            post_bytes,
            post_array,
        )

        # OmniParser element enrichment (optional)
        elements: list[GroundingElement] = []
        if self._omniparser is not None:
            try:
                regions = self._omniparser.get_interactive_regions(self._pre_array)
                elements = [
                    GroundingElement(
                        role="interactive",
                        text=None,
                        bbox=(r[0], r[1], r[2], r[3]),
                        interactable=True,
                    )
                    for r in regions
                ]
            except Exception:
                logger.debug("OmniParser enrichment failed", exc_info=True)

        # Build action
        action = GroundingAction(
            type=record.action_type.lower(),
            target_bbox=self._click_to_bbox(record.clicked_location),
            typed_text=getattr(record, "typed_text", None),
            success=success,
            success_source=success_source,
        )

        # Viewport dims from pre-screenshot array (H, W, C)
        h, w = self._pre_array.shape[:2]

        grounding_record = GroundingRecord(
            image_hash="",  # computed by writer
            image_path="",  # computed by writer
            viewport_width=w,
            viewport_height=h,
            elements=elements,
            action=action,
            source="dynamic",
            timestamp=datetime.now(timezone.utc).isoformat(),
            session_id=self._session_id,
        )

        written = self._writer.write(grounding_record, self._pre_png_bytes)
        if written:
            self._record_count += 1

    # ------------------------------------------------------------------
    # Success labelling
    # ------------------------------------------------------------------

    def _compute_success_label(
        self,
        record: ActionExecutionRecord,
        pre_bytes: bytes | None,
        pre_array: np.ndarray | None,
        post_bytes: bytes,
        post_array: np.ndarray,
    ) -> tuple[bool | None, str]:
        """Determine action success via WSM → pixel-diff → record flag."""

        # Strategy 1: World State Verifier
        if self._wsm_client is not None:
            try:
                intent = f"{record.action_type} on {record.config}"
                verdict = self._wsm_client.verify(pre_bytes, post_bytes, intent)
                return verdict.status == "pass", "wsm"
            except Exception:
                logger.debug("WSM verdict failed, falling back", exc_info=True)

        # Strategy 2: Pixel diff around click target
        if (
            record.clicked_location is not None
            and pre_array is not None
            and pre_array.shape == post_array.shape
        ):
            x, y = record.clicked_location
            h, w = pre_array.shape[:2]
            y0 = max(0, y - _DIFF_REGION_HALF)
            y1 = min(h, y + _DIFF_REGION_HALF)
            x0 = max(0, x - _DIFF_REGION_HALF)
            x1 = min(w, x + _DIFF_REGION_HALF)

            pre_region = pre_array[y0:y1, x0:x1].astype(np.float32)
            post_region = post_array[y0:y1, x0:x1].astype(np.float32)

            if pre_region.size > 0 and post_region.size > 0:
                diff = float(np.mean(np.abs(pre_region - post_region)))
                return diff > _PIXEL_DIFF_THRESHOLD, "pixel_diff"

        # Strategy 3: Record's own flag
        return record.success, "record_flag"

    # ------------------------------------------------------------------
    # Helpers
    # ------------------------------------------------------------------

    @staticmethod
    def _capture_screen() -> tuple[bytes, np.ndarray]:
        """Capture the primary monitor as PNG bytes and a BGR numpy array."""
        import mss

        with mss.mss() as sct:
            mon = sct.monitors[1]
            shot = sct.grab(mon)
            arr = np.array(shot)[:, :, :3]  # BGRA → BGR

        # Encode as PNG
        img = Image.fromarray(arr[..., ::-1])  # BGR → RGB for PIL
        buf = io.BytesIO()
        img.save(buf, format="PNG")
        return buf.getvalue(), arr

    @staticmethod
    def _click_to_bbox(
        clicked_location: tuple[int, int] | None,
    ) -> tuple[int, int, int, int] | None:
        """Convert a click coordinate to a 50×50 bounding box centred on it."""
        if clicked_location is None:
            return None
        x, y = clicked_location
        return (x - 25, y - 25, 50, 50)
