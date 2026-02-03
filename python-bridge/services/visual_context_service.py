"""Visual Context Service for qontinui-runner.

This service provides AI-friendly visual context generation including:
- Annotated screenshots with element IDs and bounding boxes
- Visual diffs between screenshots
- Interaction heatmaps showing clickable regions

The service integrates with ScreenshotService for storage and provides
methods that can be called from the Rust backend via IPC.
"""

from __future__ import annotations

import base64
import json
import logging
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import cv2
import numpy as np
from qontinui.discovery import (
    VisualContextGenerator,
)
from qontinui.discovery.element_detection import DetectedElement

from .screenshot_service import ScreenshotService

logger = logging.getLogger(__name__)


@dataclass
class VisualContextResult:
    """Result from a visual context operation.

    Attributes:
        success: Whether the operation succeeded
        data: Result data (type varies by operation)
        file_path: Path where result was saved (if applicable)
        error: Error message if operation failed
        duration_ms: Time taken to complete the operation
    """

    success: bool
    data: dict[str, Any] | None = None
    file_path: str | None = None
    error: str | None = None
    duration_ms: float = 0.0


class VisualContextService:
    """Service for generating visual context for AI consumption.

    This service wraps the VisualContextGenerator and integrates with
    the ScreenshotService for storage. It provides high-level methods
    for the runner's Rust backend to call.

    Example:
        >>> service = VisualContextService(storage_dir=Path(".dev-logs"))
        >>> result = await service.capture_and_annotate()
        >>> if result.success:
        ...     print(f"Saved to: {result.file_path}")
    """

    def __init__(
        self,
        storage_dir: Path,
        screenshot_service: ScreenshotService | None = None,
        enabled: bool = True,
    ) -> None:
        """Initialize the visual context service.

        Args:
            storage_dir: Directory for storing generated visual context files
            screenshot_service: Optional ScreenshotService for screenshot management
            enabled: Whether the service is enabled (if False, operations are no-ops)
        """
        self.storage_dir = storage_dir
        self.enabled = enabled
        self.visual_context_dir = storage_dir / "visual_context"

        # Create the visual context generator
        self.generator = VisualContextGenerator()

        # Use provided screenshot service or create one
        self.screenshot_service = screenshot_service or ScreenshotService(
            storage_dir=storage_dir, enabled=enabled
        )

        # Create directory structure if enabled
        if self.enabled:
            self.visual_context_dir.mkdir(parents=True, exist_ok=True)

        # Counter for sequential file naming
        self._snapshot_counter = 0
        self._diff_counter = 0
        self._heatmap_counter = 0

    def capture_and_annotate(
        self,
        screenshot: np.ndarray | bytes | str,
        elements: list[dict[str, Any]] | None = None,
        auto_detect: bool = True,
        include_legend: bool = True,
        save_to_file: bool = True,
    ) -> VisualContextResult:
        """Capture a screenshot and annotate it with element IDs.

        Args:
            screenshot: Screenshot as numpy array, bytes (PNG), or base64 string
            elements: Optional list of element dictionaries with bounding boxes
            auto_detect: If True and no elements, auto-detect elements
            include_legend: If True, include color legend in output
            save_to_file: If True, save the annotated image to a file

        Returns:
            VisualContextResult with annotated snapshot data
        """
        if not self.enabled:
            return VisualContextResult(success=False, error="Service is disabled")

        start_time = time.perf_counter()

        try:
            # Convert screenshot to numpy array
            screenshot_array = self._to_numpy(screenshot)

            # Convert element dictionaries to DetectedElement objects
            detected_elements = None
            if elements:
                detected_elements = self._elements_from_dicts(elements)

            # Generate annotated snapshot
            snapshot = self.generator.generate_annotated_snapshot(
                screenshot=screenshot_array,
                elements=detected_elements,
                auto_detect=auto_detect,
                include_legend=include_legend,
            )

            # Optionally save to file
            file_path = None
            if save_to_file:
                self._snapshot_counter += 1
                filename = f"annotated-{self._snapshot_counter:04d}.png"
                file_path = str(self.visual_context_dir / filename)
                snapshot.save(file_path)

                # Also save metadata
                metadata_path = (
                    self.visual_context_dir / f"annotated-{self._snapshot_counter:04d}.json"
                )
                metadata_path.write_text(json.dumps(snapshot.to_dict(), indent=2), encoding="utf-8")

            duration_ms = (time.perf_counter() - start_time) * 1000

            return VisualContextResult(
                success=True,
                data=snapshot.to_dict(),
                file_path=file_path,
                duration_ms=duration_ms,
            )

        except Exception as e:
            logger.exception("Failed to capture and annotate screenshot")
            return VisualContextResult(
                success=False,
                error=str(e),
                duration_ms=(time.perf_counter() - start_time) * 1000,
            )

    def compare_states(
        self,
        before: np.ndarray | bytes | str,
        after: np.ndarray | bytes | str,
        threshold: float = 30.0,
        min_region_area: int = 100,
        save_to_file: bool = True,
    ) -> VisualContextResult:
        """Compare two screenshots and generate a visual diff.

        Args:
            before: Screenshot before the change
            after: Screenshot after the change
            threshold: Pixel difference threshold for considering a change
            min_region_area: Minimum area in pixels to consider a changed region
            save_to_file: If True, save the diff image to a file

        Returns:
            VisualContextResult with visual diff data
        """
        if not self.enabled:
            return VisualContextResult(success=False, error="Service is disabled")

        start_time = time.perf_counter()

        try:
            # Convert screenshots to numpy arrays
            before_array = self._to_numpy(before)
            after_array = self._to_numpy(after)

            # Generate visual diff
            diff = self.generator.generate_visual_diff(
                before=before_array,
                after=after_array,
                threshold=threshold,
                min_region_area=min_region_area,
            )

            # Optionally save to file
            file_path = None
            if save_to_file:
                self._diff_counter += 1
                filename = f"diff-{self._diff_counter:04d}.png"
                file_path = str(self.visual_context_dir / filename)
                diff.save(file_path)

                # Also save metadata
                metadata_path = self.visual_context_dir / f"diff-{self._diff_counter:04d}.json"
                metadata_path.write_text(json.dumps(diff.to_dict(), indent=2), encoding="utf-8")

            duration_ms = (time.perf_counter() - start_time) * 1000

            return VisualContextResult(
                success=True,
                data=diff.to_dict(),
                file_path=file_path,
                duration_ms=duration_ms,
            )

        except Exception as e:
            logger.exception("Failed to compare states")
            return VisualContextResult(
                success=False,
                error=str(e),
                duration_ms=(time.perf_counter() - start_time) * 1000,
            )

    def generate_heatmap(
        self,
        screenshot: np.ndarray | bytes | str,
        elements: list[dict[str, Any]] | None = None,
        auto_detect: bool = True,
        alpha: float = 0.6,
        save_to_file: bool = True,
    ) -> VisualContextResult:
        """Generate an interaction heatmap showing clickable regions.

        Args:
            screenshot: Screenshot to analyze
            elements: Optional list of element dictionaries with bounding boxes
            auto_detect: If True and no elements, auto-detect elements
            alpha: Transparency of the heatmap overlay (0-1)
            save_to_file: If True, save the heatmap image to a file

        Returns:
            VisualContextResult with heatmap data
        """
        if not self.enabled:
            return VisualContextResult(success=False, error="Service is disabled")

        start_time = time.perf_counter()

        try:
            # Convert screenshot to numpy array
            screenshot_array = self._to_numpy(screenshot)

            # Convert element dictionaries to DetectedElement objects
            detected_elements = None
            if elements:
                detected_elements = self._elements_from_dicts(elements)

            # Generate interaction heatmap
            heatmap = self.generator.generate_interaction_heatmap(
                screenshot=screenshot_array,
                elements=detected_elements,
                auto_detect=auto_detect,
                alpha=alpha,
            )

            # Optionally save to file
            file_path = None
            if save_to_file:
                self._heatmap_counter += 1
                filename = f"heatmap-{self._heatmap_counter:04d}.png"
                file_path = str(self.visual_context_dir / filename)
                heatmap.save(file_path)

                # Also save metadata
                metadata_path = (
                    self.visual_context_dir / f"heatmap-{self._heatmap_counter:04d}.json"
                )
                metadata_path.write_text(json.dumps(heatmap.to_dict(), indent=2), encoding="utf-8")

            duration_ms = (time.perf_counter() - start_time) * 1000

            return VisualContextResult(
                success=True,
                data=heatmap.to_dict(),
                file_path=file_path,
                duration_ms=duration_ms,
            )

        except Exception as e:
            logger.exception("Failed to generate heatmap")
            return VisualContextResult(
                success=False,
                error=str(e),
                duration_ms=(time.perf_counter() - start_time) * 1000,
            )

    def _to_numpy(self, image: np.ndarray | bytes | str) -> np.ndarray:
        """Convert image to numpy array.

        Args:
            image: Image as numpy array, bytes (PNG), or base64 string

        Returns:
            Numpy array in BGR format

        Raises:
            ValueError: If image type is unsupported or decoding fails
        """
        if isinstance(image, np.ndarray):
            return image

        # Handle bytes
        if isinstance(image, bytes):
            nparr = np.frombuffer(image, np.uint8)
            decoded = cv2.imdecode(nparr, cv2.IMREAD_COLOR)
            if decoded is None:
                raise ValueError("Failed to decode image from bytes")
            return decoded

        # Handle base64 string
        if isinstance(image, str):
            # Remove data URL prefix if present
            if image.startswith("data:"):
                image = image.split(",", 1)[1]

            # Decode base64
            image_bytes = base64.b64decode(image)
            nparr = np.frombuffer(image_bytes, np.uint8)
            decoded = cv2.imdecode(nparr, cv2.IMREAD_COLOR)
            if decoded is None:
                raise ValueError("Failed to decode image from base64 string")
            return decoded

        raise ValueError(f"Unsupported image type: {type(image)}")

    def _elements_from_dicts(self, elements: list[dict[str, Any]]) -> list[DetectedElement]:
        """Convert element dictionaries to DetectedElement objects.

        Args:
            elements: List of element dictionaries

        Returns:
            List of DetectedElement objects
        """
        from qontinui.discovery.element_detection import BoundingBox, DetectedElement

        result = []
        for elem in elements:
            # Handle both flat and nested bounding box formats
            if "bounds" in elem:
                bounds = elem["bounds"]
                x, y = bounds.get("x", 0), bounds.get("y", 0)
                w, h = bounds.get("width", 0), bounds.get("height", 0)
            elif "bounding_box" in elem:
                bbox = elem["bounding_box"]
                x, y = bbox.get("x", 0), bbox.get("y", 0)
                w, h = bbox.get("width", 0), bbox.get("height", 0)
            else:
                x = elem.get("x", 0)
                y = elem.get("y", 0)
                w = elem.get("width", elem.get("w", 0))
                h = elem.get("height", elem.get("h", 0))

            result.append(
                DetectedElement(
                    bounding_box=BoundingBox(x=int(x), y=int(y), width=int(w), height=int(h)),
                    confidence=elem.get("confidence", 1.0),
                    label=elem.get("label"),
                    element_type=elem.get("type", elem.get("element_type")),
                )
            )

        return result

    def cleanup_old_files(self, keep_last_n: int = 50) -> int:
        """Clean up old visual context files.

        Args:
            keep_last_n: Number of most recent files to keep

        Returns:
            Number of files deleted
        """
        if not self.enabled:
            return 0

        deleted_count = 0

        for pattern in ["annotated-*.png", "diff-*.png", "heatmap-*.png"]:
            files = sorted(self.visual_context_dir.glob(pattern))
            if len(files) > keep_last_n:
                for file_path in files[: len(files) - keep_last_n]:
                    # Delete image file
                    file_path.unlink(missing_ok=True)
                    # Delete corresponding metadata
                    metadata_path = file_path.with_suffix(".json")
                    metadata_path.unlink(missing_ok=True)
                    deleted_count += 1

        return deleted_count
