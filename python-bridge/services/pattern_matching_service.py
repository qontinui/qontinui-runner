"""
Pattern Matching Service

Provides pattern matching functionality via the qontinui library's Find system.
Supports both find (best match) and find_all operations.
"""

import base64
import io
import logging
import os
import time
from collections.abc import Callable
from dataclasses import dataclass
from typing import Any

import cv2
import numpy as np
from PIL import Image as PILImage

logger = logging.getLogger(__name__)

# Check if qontinui library is available
QONTINUI_AVAILABLE = False
_get_hal_func: Callable[[], Any] | None = None

try:
    from qontinui.find.find import Find  # noqa: F401
    from qontinui.model.element import Pattern, Region  # noqa: F401

    QONTINUI_AVAILABLE = True
    logger.info("qontinui library loaded for pattern matching")

    # get_hal may not exist in all versions - import cautiously
    try:
        from qontinui.hal import get_hal as _imported_get_hal  # type: ignore[attr-defined]

        _get_hal_func = _imported_get_hal
    except (ImportError, AttributeError):
        pass  # _get_hal_func stays None
except ImportError as e:
    logger.warning(f"qontinui library not available for pattern matching: {e}")


@dataclass
class MatchResult:
    """Result of a pattern match."""

    x: int
    y: int
    width: int
    height: int
    similarity: float
    center_x: int
    center_y: int


@dataclass
class PatternMatchResponse:
    """Response from pattern matching operation."""

    success: bool
    matches: list[dict[str, Any]]
    search_time_ms: float
    screenshot_width: int
    screenshot_height: int
    template_width: int
    template_height: int
    error: str | None = None


class PatternMatchingService:
    """Service for pattern matching operations."""

    def __init__(self) -> None:
        """Initialize the pattern matching service."""
        self._hal = None
        self._invariant_matcher: Any | None = None
        if QONTINUI_AVAILABLE and _get_hal_func is not None:
            try:
                self._hal = _get_hal_func()
                logger.info("HAL initialized for pattern matching")
            except Exception as e:
                logger.error(f"Failed to initialize HAL: {e}")

    def _decode_base64_image(self, base64_str: str) -> np.ndarray:
        """Decode a base64 image string to numpy array.

        Args:
            base64_str: Base64 encoded image (may include data URI prefix)

        Returns:
            OpenCV image array (BGR format)
        """
        # Remove data URI prefix if present
        if "," in base64_str:
            base64_str = base64_str.split(",", 1)[1]

        # Decode base64 to bytes
        image_bytes = base64.b64decode(base64_str)

        # Convert to PIL Image
        pil_image: PILImage.Image = PILImage.open(io.BytesIO(image_bytes))

        # Convert to RGB if needed (handles RGBA, L, etc.)
        if pil_image.mode != "RGB":
            pil_image = pil_image.convert("RGB")

        # Convert to numpy array (RGB)
        rgb_array = np.array(pil_image)

        # Convert RGB to BGR for OpenCV
        bgr_array = cv2.cvtColor(rgb_array, cv2.COLOR_RGB2BGR)

        return bgr_array

    def _load_image_from_path(self, path: str) -> np.ndarray | None:
        """Load an image from file path.

        Args:
            path: Path to image file

        Returns:
            OpenCV image array or None if failed
        """
        try:
            img = cv2.imread(path)
            if img is None:
                logger.error(f"Failed to load image from path: {path}")
            return img
        except Exception as e:
            logger.error(f"Error loading image from {path}: {e}")
            return None

    async def find(
        self,
        screenshot: str,
        template: str,
        similarity: float = 0.8,
        search_region: dict[str, int] | None = None,
        invariant: bool = False,
        invariant_scales: list[float] | None = None,
    ) -> PatternMatchResponse:
        """Find the best match of template in screenshot.

        Args:
            screenshot: Base64 encoded screenshot or file path
            template: Base64 encoded template image or file path
            similarity: Minimum similarity threshold (0.0 to 1.0)
            search_region: Optional region to search within {x, y, width, height}
            invariant: If True, use scale-invariant matching (slower but
                       handles DPI differences)
            invariant_scales: Custom scale factors for invariant matching

        Returns:
            PatternMatchResponse with best match or empty matches list
        """
        return await self._find_internal(
            screenshot=screenshot,
            template=template,
            similarity=similarity,
            search_region=search_region,
            find_all=False,
            invariant=invariant,
            invariant_scales=invariant_scales,
        )

    async def find_all(
        self,
        screenshot: str,
        template: str,
        similarity: float = 0.8,
        search_region: dict[str, int] | None = None,
        max_matches: int = 100,
        invariant: bool = False,
        invariant_scales: list[float] | None = None,
    ) -> PatternMatchResponse:
        """Find all matches of template in screenshot.

        Args:
            screenshot: Base64 encoded screenshot or file path
            template: Base64 encoded template image or file path
            similarity: Minimum similarity threshold (0.0 to 1.0)
            search_region: Optional region to search within {x, y, width, height}
            max_matches: Maximum number of matches to return
            invariant: If True, use scale-invariant matching
            invariant_scales: Custom scale factors for invariant matching

        Returns:
            PatternMatchResponse with all matches
        """
        return await self._find_internal(
            screenshot=screenshot,
            template=template,
            similarity=similarity,
            search_region=search_region,
            find_all=True,
            max_matches=max_matches,
            invariant=invariant,
            invariant_scales=invariant_scales,
        )

    async def _find_internal(
        self,
        screenshot: str,
        template: str,
        similarity: float,
        search_region: dict[str, int] | None,
        find_all: bool,
        max_matches: int = 100,
        invariant: bool = False,
        invariant_scales: list[float] | None = None,
    ) -> PatternMatchResponse:
        """Internal find implementation using OpenCV template matching.

        Uses OpenCV's matchTemplate directly for reliable results.
        When *invariant* is True, delegates to the HAL's
        ``find_template_invariant()`` for scale-aware matching.
        """
        start_time = time.time()

        try:
            # Load screenshot
            if screenshot.startswith(("http://", "https://")) or os.path.isabs(screenshot):
                screenshot_img = self._load_image_from_path(screenshot)
                if screenshot_img is None:
                    return PatternMatchResponse(
                        success=False,
                        matches=[],
                        search_time_ms=0,
                        screenshot_width=0,
                        screenshot_height=0,
                        template_width=0,
                        template_height=0,
                        error=f"Failed to load screenshot from path: {screenshot}",
                    )
            else:
                screenshot_img = self._decode_base64_image(screenshot)

            # Load template
            if template.startswith(("http://", "https://")) or os.path.isabs(template):
                template_img = self._load_image_from_path(template)
                if template_img is None:
                    return PatternMatchResponse(
                        success=False,
                        matches=[],
                        search_time_ms=0,
                        screenshot_width=screenshot_img.shape[1],
                        screenshot_height=screenshot_img.shape[0],
                        template_width=0,
                        template_height=0,
                        error=f"Failed to load template from path: {template}",
                    )
            else:
                template_img = self._decode_base64_image(template)

            screenshot_height, screenshot_width = screenshot_img.shape[:2]
            template_height, template_width = template_img.shape[:2]

            # Apply search region if specified
            search_img = screenshot_img
            offset_x, offset_y = 0, 0

            if search_region:
                x = max(0, search_region.get("x", 0))
                y = max(0, search_region.get("y", 0))
                w = search_region.get("width", screenshot_width - x)
                h = search_region.get("height", screenshot_height - y)

                # Ensure region is within bounds
                x = min(x, screenshot_width - 1)
                y = min(y, screenshot_height - 1)
                w = min(w, screenshot_width - x)
                h = min(h, screenshot_height - y)

                if w >= template_width and h >= template_height:
                    search_img = screenshot_img[y : y + h, x : x + w]
                    offset_x, offset_y = x, y
                else:
                    logger.warning(
                        f"Search region ({w}x{h}) smaller than template ({template_width}x{template_height})"
                    )

            # Check if template is larger than search area (skip for invariant
            # matching which handles scale differences internally)
            search_h, search_w = search_img.shape[:2]
            if not invariant and (template_width > search_w or template_height > search_h):
                return PatternMatchResponse(
                    success=True,
                    matches=[],
                    search_time_ms=(time.time() - start_time) * 1000,
                    screenshot_width=screenshot_width,
                    screenshot_height=screenshot_height,
                    template_width=template_width,
                    template_height=template_height,
                    error=None,
                )

            matches: list[dict[str, Any]] = []

            # Scale-invariant matching via HAL
            if invariant:
                try:
                    from qontinui.hal.implementations.opencv_matcher import OpenCVMatcher

                    if self._invariant_matcher is None:
                        self._invariant_matcher = OpenCVMatcher()
                    matcher = self._invariant_matcher
                    # Convert BGR numpy arrays to PIL for HAL interface
                    haystack_pil = PILImage.fromarray(cv2.cvtColor(search_img, cv2.COLOR_BGR2RGB))
                    needle_pil = PILImage.fromarray(cv2.cvtColor(template_img, cv2.COLOR_BGR2RGB))

                    if find_all:
                        hal_matches = matcher.find_all_template_invariant(
                            haystack=haystack_pil,
                            needle=needle_pil,
                            scales=invariant_scales,
                            confidence=similarity,
                            limit=max_matches,
                        )
                        for m in hal_matches:
                            matches.append(
                                {
                                    "x": int(m.x + offset_x),
                                    "y": int(m.y + offset_y),
                                    "width": m.width,
                                    "height": m.height,
                                    "similarity": m.confidence,
                                }
                            )
                    else:
                        hal_match = matcher.find_template_invariant(
                            haystack=haystack_pil,
                            needle=needle_pil,
                            scales=invariant_scales,
                            confidence=similarity,
                        )
                        if hal_match is not None:
                            matches.append(
                                {
                                    "x": int(hal_match.x + offset_x),
                                    "y": int(hal_match.y + offset_y),
                                    "width": hal_match.width,
                                    "height": hal_match.height,
                                    "similarity": hal_match.confidence,
                                }
                            )
                except ImportError:
                    logger.warning(
                        "Invariant matching requested but qontinui HAL unavailable, "
                        "falling back to standard matching"
                    )
                    invariant = False

            # Standard template matching
            if not invariant:
                result = cv2.matchTemplate(search_img, template_img, cv2.TM_CCOEFF_NORMED)

            if not invariant:
                if find_all:
                    # Find all matches above threshold
                    locations = np.where(result >= similarity)

                    # Group nearby matches using non-maximum suppression
                    match_data = []
                    for pt in zip(*locations[::-1], strict=False):  # Switch to (x, y) format
                        match_similarity = float(result[pt[1], pt[0]])
                        match_data.append(
                            {
                                "x": int(pt[0] + offset_x),
                                "y": int(pt[1] + offset_y),
                                "width": template_width,
                                "height": template_height,
                                "similarity": match_similarity,
                            }
                        )

                    # Apply NMS to remove overlapping matches
                    matches = self._apply_nms(match_data, iou_threshold=0.5)

                    # Sort by similarity (descending) and limit
                    matches.sort(key=lambda m: m["similarity"], reverse=True)
                    matches = matches[:max_matches]
                else:
                    # Find single best match
                    min_val, max_val, min_loc, max_loc = cv2.minMaxLoc(result)

                    if max_val >= similarity:
                        matches.append(
                            {
                                "x": int(max_loc[0] + offset_x),
                                "y": int(max_loc[1] + offset_y),
                                "width": template_width,
                                "height": template_height,
                                "similarity": float(max_val),
                            }
                        )

            # Add center coordinates
            for match in matches:
                match["center_x"] = match["x"] + match["width"] // 2
                match["center_y"] = match["y"] + match["height"] // 2

            search_time_ms = (time.time() - start_time) * 1000

            return PatternMatchResponse(
                success=True,
                matches=matches,
                search_time_ms=search_time_ms,
                screenshot_width=screenshot_width,
                screenshot_height=screenshot_height,
                template_width=template_width,
                template_height=template_height,
            )

        except Exception as e:
            logger.exception(f"Pattern matching failed: {e}")
            return PatternMatchResponse(
                success=False,
                matches=[],
                search_time_ms=(time.time() - start_time) * 1000,
                screenshot_width=0,
                screenshot_height=0,
                template_width=0,
                template_height=0,
                error=str(e),
            )

    def _apply_nms(
        self, matches: list[dict[str, Any]], iou_threshold: float = 0.5
    ) -> list[dict[str, Any]]:
        """Apply non-maximum suppression to remove overlapping matches.

        Args:
            matches: List of match dictionaries with x, y, width, height, similarity
            iou_threshold: IoU threshold for suppression

        Returns:
            Filtered list of matches
        """
        if not matches:
            return []

        # Sort by similarity descending
        sorted_matches = sorted(matches, key=lambda m: m["similarity"], reverse=True)
        selected: list[dict[str, Any]] = []

        for match in sorted_matches:
            # Check if this match overlaps significantly with any selected match
            should_add = True

            for selected_match in selected:
                iou = self._calculate_iou(match, selected_match)
                if iou > iou_threshold:
                    should_add = False
                    break

            if should_add:
                selected.append(match)

        return selected

    def _calculate_iou(self, box1: dict[str, Any], box2: dict[str, Any]) -> float:
        """Calculate Intersection over Union of two boxes.

        Args:
            box1, box2: Dictionaries with x, y, width, height

        Returns:
            IoU value between 0 and 1
        """
        x1_1, y1_1 = box1["x"], box1["y"]
        x2_1, y2_1 = x1_1 + box1["width"], y1_1 + box1["height"]

        x1_2, y1_2 = box2["x"], box2["y"]
        x2_2, y2_2 = x1_2 + box2["width"], y1_2 + box2["height"]

        # Calculate intersection
        xi1, yi1 = max(x1_1, x1_2), max(y1_1, y1_2)
        xi2, yi2 = min(x2_1, x2_2), min(y2_1, y2_2)

        if xi2 <= xi1 or yi2 <= yi1:
            return 0.0

        intersection = (xi2 - xi1) * (yi2 - yi1)

        # Calculate union
        area1 = box1["width"] * box1["height"]
        area2 = box2["width"] * box2["height"]
        union = area1 + area2 - intersection

        return intersection / union if union > 0 else 0.0


# Singleton instance
_pattern_matching_service: PatternMatchingService | None = None


def get_pattern_matching_service() -> PatternMatchingService:
    """Get the singleton PatternMatchingService instance."""
    global _pattern_matching_service
    if _pattern_matching_service is None:
        _pattern_matching_service = PatternMatchingService()
    return _pattern_matching_service
