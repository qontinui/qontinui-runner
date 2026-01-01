"""Verification Service for AI Verification Agent.

This service provides state detection and element verification capabilities
for the AI Verification Agent in the qontinui-runner. It integrates with
the qontinui library's state detection and pattern matching functionality.

The service supports:
1. State detection - Determining which states are currently active based on
   identifying images/patterns
2. Element verification - Checking for expected and unexpected visual elements
3. Screenshot capture - Capturing screenshots for verification documentation

Usage:
    This service is used by the Rust verification_task.rs via the Python bridge.
    Commands are sent via stdin/stdout JSON communication.
"""

from __future__ import annotations

import asyncio
import base64
import io
import logging
import time
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, Any

logger = logging.getLogger(__name__)

if TYPE_CHECKING:
    from qontinui.action_executors import DelegatingActionExecutor
    from qontinui.json_executor.state_executor import StateExecutor

try:
    from qontinui import registry
    from qontinui.actions.find import FindAction, FindOptions
    from qontinui.hal.factory import HALFactory
    from qontinui.model.element import Pattern

    QONTINUI_AVAILABLE = True
except ImportError:
    QONTINUI_AVAILABLE = False
    logger.debug("Qontinui library not available for verification service")


@dataclass
class StateDetectionResult:
    """Result of state detection operation."""

    state_id: str
    state_name: str
    detected: bool
    confidence: float
    matched_images: list[str]
    missing_images: list[str]
    detection_time_ms: float


@dataclass
class ElementVerificationResult:
    """Result of element verification operation."""

    element_id: str
    element_name: str
    found: bool
    expected: bool
    confidence: float
    location: dict[str, int] | None
    verification_time_ms: float


class VerificationService:
    """Service for state detection and element verification.

    This service provides the core verification functionality used by
    the AI Verification Agent. It uses the qontinui library's pattern
    matching and state detection capabilities.

    Responsibilities:
    - Detect which states are currently active on screen
    - Verify presence/absence of specific visual elements
    - Capture screenshots for verification documentation
    - Provide confidence scores for detections
    """

    def __init__(
        self,
        state_executor: StateExecutor | None = None,
        action_executor: DelegatingActionExecutor | None = None,
        config: dict[str, Any] | None = None,
    ) -> None:
        """Initialize the verification service.

        Args:
            state_executor: StateExecutor from qontinui for state management
            action_executor: DelegatingActionExecutor for pattern matching
            config: Loaded automation configuration
        """
        self.state_executor = state_executor
        self.action_executor = action_executor
        self.config = config
        self._screenshot_dir: Path | None = None

    def set_screenshot_directory(self, directory: str | Path) -> None:
        """Set the directory for saving verification screenshots.

        Args:
            directory: Path to screenshot storage directory
        """
        self._screenshot_dir = Path(directory)
        self._screenshot_dir.mkdir(parents=True, exist_ok=True)

    def detect_current_states(
        self,
        state_ids: list[str],
        monitor_index: int | None = None,
    ) -> dict[str, Any]:
        """Detect which of the specified states are currently active.

        This method checks each specified state by looking for its
        identifying images on the screen. A state is considered active
        if all its required identifying images are found.

        Args:
            state_ids: List of state IDs to check for
            monitor_index: Optional monitor index for multi-monitor setups

        Returns:
            Dictionary containing:
            - success: Whether the operation completed
            - detected_states: List of states that were detected as active
            - detection_details: Per-state detection results with confidence
            - screenshot_base64: Optional base64 encoded screenshot
            - detection_time_ms: Total detection time
        """
        if not QONTINUI_AVAILABLE:
            return {
                "success": False,
                "error": "Qontinui library not available",
                "detected_states": [],
                "detection_details": [],
            }

        if not self.config:
            return {
                "success": False,
                "error": "Configuration not loaded",
                "detected_states": [],
                "detection_details": [],
            }

        start_time = time.time()
        detected_states: list[str] = []
        detection_details: list[dict[str, Any]] = []

        # Get state configuration
        states_config = self.config.get("states", [])
        state_map = {s.get("id"): s for s in states_config}

        for state_id in state_ids:
            state_config = state_map.get(state_id)
            if not state_config:
                detection_details.append(
                    {
                        "state_id": state_id,
                        "state_name": "Unknown",
                        "detected": False,
                        "confidence": 0.0,
                        "error": f"State {state_id} not found in configuration",
                        "matched_images": [],
                        "missing_images": [],
                    }
                )
                continue

            state_name = state_config.get("name", state_id)
            state_images = state_config.get("stateImages", [])

            # Check identifying images for this state
            result = self._check_state_images(
                state_id=state_id,
                state_name=state_name,
                state_images=state_images,
                monitor_index=monitor_index,
            )

            detection_details.append(result)
            if result["detected"]:
                detected_states.append(state_id)

        total_time = (time.time() - start_time) * 1000

        # Capture screenshot if directory is configured
        screenshot_base64 = None
        if self._screenshot_dir:
            screenshot_base64 = self._capture_screenshot_base64(monitor_index)

        return {
            "success": True,
            "detected_states": detected_states,
            "detection_details": detection_details,
            "screenshot_base64": screenshot_base64,
            "detection_time_ms": total_time,
        }

    def _check_state_images(
        self,
        state_id: str,
        state_name: str,
        state_images: list[dict[str, Any]],
        monitor_index: int | None = None,
    ) -> dict[str, Any]:
        """Check if a state's identifying images are visible.

        Args:
            state_id: State identifier
            state_name: Human-readable state name
            state_images: List of state image configurations
            monitor_index: Optional monitor index

        Returns:
            Detection result dictionary
        """
        start_time = time.time()
        matched_images: list[str] = []
        missing_images: list[str] = []
        confidences: list[float] = []

        # Filter to identifying images (those marked as required or identifying)
        identifying_images = [
            img
            for img in state_images
            if img.get("isIdentifying", False) or img.get("required", False)
        ]

        if not identifying_images:
            # No identifying images, consider state potentially active
            return {
                "state_id": state_id,
                "state_name": state_name,
                "detected": True,
                "confidence": 0.5,  # Low confidence since no images to verify
                "matched_images": [],
                "missing_images": [],
                "detection_time_ms": (time.time() - start_time) * 1000,
            }

        action = FindAction()

        for state_image in identifying_images:
            image_id = state_image.get("id")
            image_name = state_image.get("name", image_id)
            threshold = state_image.get("threshold", 0.8)

            # Get pattern from registry
            patterns = state_image.get("patterns", [])
            if not patterns:
                missing_images.append(image_id)
                continue

            # Try to find the first pattern
            pattern_config = patterns[0]
            pattern_image_id = pattern_config.get("imageId") or pattern_config.get("image")

            if not pattern_image_id:
                missing_images.append(image_id)
                continue

            # Get image from registry
            image_obj = registry.get_image(pattern_image_id)
            if not image_obj:
                missing_images.append(image_id)
                continue

            # Create pattern for matching
            image_metadata = registry.get_image_metadata(pattern_image_id)
            file_path = image_metadata.get("file_path") if image_metadata else None

            if not file_path:
                missing_images.append(image_id)
                continue

            pattern = Pattern.from_file(
                img_path=str(file_path),
                name=image_name,
            ).with_similarity(threshold)

            # Perform find operation
            find_result = action.find(
                pattern=pattern,
                options=FindOptions(
                    similarity=threshold,
                    find_all=False,
                    monitor_index=monitor_index,
                ),
            )

            if find_result.found and find_result.matches:
                matched_images.append(image_id)
                # Get confidence from best match
                best_match = find_result.matches[0]
                confidences.append(best_match.score if hasattr(best_match, "score") else threshold)
            else:
                missing_images.append(image_id)

        # State is detected if all identifying images are found
        detected = len(missing_images) == 0 and len(matched_images) > 0
        avg_confidence = sum(confidences) / len(confidences) if confidences else 0.0

        return {
            "state_id": state_id,
            "state_name": state_name,
            "detected": detected,
            "confidence": avg_confidence,
            "matched_images": matched_images,
            "missing_images": missing_images,
            "detection_time_ms": (time.time() - start_time) * 1000,
        }

    def verify_elements(
        self,
        expected_elements: list[str],
        unexpected_elements: list[str],
        monitor_index: int | None = None,
    ) -> dict[str, Any]:
        """Verify presence of expected elements and absence of unexpected ones.

        Args:
            expected_elements: List of image/element IDs that should be visible
            unexpected_elements: List of image/element IDs that should NOT be visible
            monitor_index: Optional monitor index for multi-monitor setups

        Returns:
            Dictionary containing:
            - success: Whether the operation completed
            - all_expected_found: True if all expected elements were found
            - no_unexpected_found: True if no unexpected elements were found
            - expected_results: Per-element results for expected elements
            - unexpected_results: Per-element results for unexpected elements
            - verification_time_ms: Total verification time
        """
        if not QONTINUI_AVAILABLE:
            return {
                "success": False,
                "error": "Qontinui library not available",
                "all_expected_found": False,
                "no_unexpected_found": True,
                "expected_results": [],
                "unexpected_results": [],
            }

        start_time = time.time()
        expected_results: list[dict[str, Any]] = []
        unexpected_results: list[dict[str, Any]] = []

        action = FindAction()

        # Check expected elements
        for element_id in expected_elements:
            result = self._verify_single_element(
                element_id=element_id,
                action=action,
                monitor_index=monitor_index,
                expected=True,
            )
            expected_results.append(result)

        # Check unexpected elements
        for element_id in unexpected_elements:
            result = self._verify_single_element(
                element_id=element_id,
                action=action,
                monitor_index=monitor_index,
                expected=False,
            )
            unexpected_results.append(result)

        all_expected_found = all(r["found"] for r in expected_results) if expected_results else True
        no_unexpected_found = not any(r["found"] for r in unexpected_results)

        total_time = (time.time() - start_time) * 1000

        return {
            "success": True,
            "all_expected_found": all_expected_found,
            "no_unexpected_found": no_unexpected_found,
            "expected_results": expected_results,
            "unexpected_results": unexpected_results,
            "verification_time_ms": total_time,
        }

    def _verify_single_element(
        self,
        element_id: str,
        action: FindAction,
        monitor_index: int | None,
        expected: bool,
    ) -> dict[str, Any]:
        """Verify presence of a single element.

        Args:
            element_id: Element/image ID to verify
            action: FindAction instance to use
            monitor_index: Monitor index for capture
            expected: Whether element is expected to be present

        Returns:
            Verification result dictionary
        """
        start_time = time.time()

        # Get image from registry
        image_obj = registry.get_image(element_id)
        if not image_obj:
            return {
                "element_id": element_id,
                "element_name": element_id,
                "found": False,
                "expected": expected,
                "confidence": 0.0,
                "location": None,
                "error": f"Element {element_id} not found in registry",
                "verification_time_ms": (time.time() - start_time) * 1000,
            }

        # Get metadata for file path and name
        image_metadata = registry.get_image_metadata(element_id)
        file_path = image_metadata.get("file_path") if image_metadata else None
        element_name = image_metadata.get("name", element_id) if image_metadata else element_id

        if not file_path:
            return {
                "element_id": element_id,
                "element_name": element_name,
                "found": False,
                "expected": expected,
                "confidence": 0.0,
                "location": None,
                "error": f"No file path for element {element_id}",
                "verification_time_ms": (time.time() - start_time) * 1000,
            }

        # Create pattern for matching
        pattern = Pattern.from_file(
            img_path=str(file_path),
            name=element_name,
        ).with_similarity(
            0.8
        )  # Default threshold

        # Perform find operation
        find_result = action.find(
            pattern=pattern,
            options=FindOptions(
                similarity=0.8,
                find_all=False,
                monitor_index=monitor_index,
            ),
        )

        found = find_result.found and bool(find_result.matches)
        confidence = 0.0
        location = None

        if found and find_result.matches:
            best_match = find_result.matches[0]
            confidence = best_match.score if hasattr(best_match, "score") else 0.8
            if hasattr(best_match, "x") and hasattr(best_match, "y"):
                location = {
                    "x": best_match.x,
                    "y": best_match.y,
                    "width": best_match.w if hasattr(best_match, "w") else 0,
                    "height": best_match.h if hasattr(best_match, "h") else 0,
                }

        return {
            "element_id": element_id,
            "element_name": element_name,
            "found": found,
            "expected": expected,
            "confidence": confidence,
            "location": location,
            "verification_time_ms": (time.time() - start_time) * 1000,
        }

    def capture_screenshot(
        self,
        context: str,
        monitor_index: int | None = None,
        save_to_file: bool = True,
    ) -> dict[str, Any]:
        """Capture a screenshot for verification documentation.

        Args:
            context: Context string for naming (e.g., state_id)
            monitor_index: Optional monitor index
            save_to_file: Whether to save to file in addition to returning base64

        Returns:
            Dictionary containing:
            - success: Whether capture succeeded
            - screenshot_base64: Base64 encoded PNG data
            - file_path: Path where screenshot was saved (if save_to_file=True)
            - width: Screenshot width in pixels
            - height: Screenshot height in pixels
        """
        if not QONTINUI_AVAILABLE:
            return {
                "success": False,
                "error": "Qontinui library not available",
            }

        try:
            screen_capture = HALFactory.get_screen_capture()
            pil_image = screen_capture.capture_screen(monitor=monitor_index)

            # Convert to PNG bytes
            buffer = io.BytesIO()
            pil_image.save(buffer, format="PNG", compress_level=6)
            buffer.seek(0)

            # Encode as base64
            screenshot_base64 = base64.b64encode(buffer.getvalue()).decode("utf-8")

            result = {
                "success": True,
                "screenshot_base64": screenshot_base64,
                "width": pil_image.width,
                "height": pil_image.height,
                "monitor": monitor_index,
            }

            # Save to file if requested and directory is configured
            if save_to_file and self._screenshot_dir:
                import time as time_module

                safe_context = context.replace("/", "_").replace("\\", "_").replace(" ", "_")
                filename = f"{safe_context}_{int(time_module.time() * 1000)}.png"
                file_path = self._screenshot_dir / filename
                file_path.write_bytes(buffer.getvalue())
                result["file_path"] = str(file_path)

            return result

        except Exception as e:
            logger.error(f"Failed to capture screenshot: {e}")
            return {
                "success": False,
                "error": str(e),
            }

    def _capture_screenshot_base64(self, monitor_index: int | None = None) -> str | None:
        """Capture screenshot and return as base64 string.

        Args:
            monitor_index: Optional monitor index

        Returns:
            Base64 encoded PNG data or None on failure
        """
        result = self.capture_screenshot(
            context="detection",
            monitor_index=monitor_index,
            save_to_file=False,
        )
        if result.get("success"):
            return result.get("screenshot_base64")
        return None
