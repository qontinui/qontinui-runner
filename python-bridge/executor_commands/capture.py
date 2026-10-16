"""Screen capture, segmentation, verification, state detection and pattern-find commands.

A mixin of ``QontinuiExecutor``. Its methods were moved verbatim from
``qontinui_executor.py`` by plan
2026-10-04-runner-python-executor-routes-118-commands-through-one-if-chain
and still read executor state through ``self``. ``COMMANDS`` maps each command
name to its handler method.
"""

import logging
import traceback
from typing import Any, ClassVar

from event_manager import EventType

from ._host import ExecutorHost
from ._shared import QONTINUI_AVAILABLE, RUNNER_PARENT_DIR, get_pattern_matching_service

logger = logging.getLogger(__name__)


class CaptureCommands(ExecutorHost):
    """Screen capture, segmentation, verification, state detection and pattern-find commands."""

    COMMANDS: ClassVar[dict[str, str]] = {
        # Screenshot capture command (for direct capture via Python)
        "capture_screenshot": "_handle_capture_screenshot",
        # Get available monitors
        "get_monitors": "_handle_get_monitors",
        # SAM3 segmentation command
        "segment_screenshot": "_handle_segment_screenshot",
        # Verification Agent commands
        "detect_current_states": "_handle_detect_current_states",
        "verify_elements": "_handle_verify_elements",
        "verification_capture_screenshot": "_handle_verification_capture_screenshot",
        # Flakiness-aware execution commands
        "get_flakiness_options": "_handle_get_flakiness_options",
        # Pattern matching commands
        "pattern_find": "_handle_pattern_find",
        "pattern_find_all": "_handle_pattern_find_all",
    }

    def _handle_capture_screenshot(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle screenshot capture command.

        This captures a screenshot using the qontinui library's HAL layer,
        which captures at physical pixel resolution (not logical/scaled).

        Args:
            params: Command parameters:
                - monitor: Monitor index (0-based), None for all monitors
                - format: Image format ("png" or "jpeg"), defaults to "png"

        Returns:
            Dictionary with:
                - success: Whether capture succeeded
                - screenshot_base64: Base64 encoded image data (if success)
                - width: Image width in pixels
                - height: Image height in pixels
                - error: Error message (if failed)
        """
        import base64
        import io
        import sys

        print(
            f"[info    ] EXECUTOR: _handle_capture_screenshot called with params: {params}",
            file=sys.stderr,
            flush=True,
        )

        try:
            if not QONTINUI_AVAILABLE:
                return {
                    "success": False,
                    "error": "Qontinui library not available",
                }

            from qontinui.hal.factory import HALFactory

            screen_capture = HALFactory.get_screen_capture()
            monitor = params.get("monitor")

            # Capture the screenshot
            pil_image = screen_capture.capture_screen(monitor=monitor)

            # Convert to PNG bytes
            buffer = io.BytesIO()
            image_format = params.get("format", "png").upper()
            if image_format == "JPEG":
                # Convert RGBA to RGB for JPEG
                if pil_image.mode == "RGBA":
                    pil_image = pil_image.convert("RGB")
                pil_image.save(buffer, format="JPEG", quality=95)
            else:
                pil_image.save(buffer, format="PNG", compress_level=6)
            buffer.seek(0)

            # Encode as base64
            screenshot_base64 = base64.b64encode(buffer.getvalue()).decode("utf-8")

            self.event_manager.emit_log(
                "info",
                f"Screenshot captured: {pil_image.width}x{pil_image.height} pixels",
            )

            # Emit the screenshot as an event for the Rust bridge
            # Note: EventType is already imported at module level
            self.event_manager.emit_event(
                EventType.SCREENSHOT_TAKEN,
                {
                    "screenshot_base64": screenshot_base64,
                    "width": pil_image.width,
                    "height": pil_image.height,
                    "monitor": monitor,
                    "format": image_format.lower(),
                },
            )

            return {
                "success": True,
                "screenshot_base64": screenshot_base64,
                "width": pil_image.width,
                "height": pil_image.height,
                "monitor": monitor,
                "format": image_format.lower(),
            }

        except Exception as e:
            print(
                f"[error   ] EXECUTOR: Failed to capture screenshot: {e}",
                file=sys.stderr,
                flush=True,
            )

            print(
                f"[error   ] EXECUTOR: Traceback: {traceback.format_exc()}",
                file=sys.stderr,
                flush=True,
            )
            self.event_manager.emit_log("error", f"Failed to capture screenshot: {e}")
            return {"success": False, "error": str(e)}

    def _handle_get_monitors(self) -> dict[str, Any]:
        """Handle get monitors command.

        Returns information about all connected monitors using the qontinui
        library's MonitorManager.

        Returns:
            Dictionary with:
                - success: True
                - monitors: List of monitor info dictionaries
                - count: Number of monitors
        """
        import sys

        print(
            "[info    ] EXECUTOR: _handle_get_monitors called",
            file=sys.stderr,
            flush=True,
        )

        try:
            if not QONTINUI_AVAILABLE:
                return {
                    "success": False,
                    "error": "Qontinui library not available",
                }

            from qontinui.monitor.monitor_manager import MonitorManager

            # Create monitor manager to detect monitors
            manager = MonitorManager()
            all_monitors = manager.get_all_monitor_info()

            # Convert to serializable format
            monitors = []
            for info in all_monitors:
                # Determine position based on x coordinate
                if len(all_monitors) == 1:
                    position = "center"
                elif info.x < 0:
                    position = "left"
                elif info.index == 0:
                    position = "center"
                else:
                    position = "right"

                monitors.append(
                    {
                        "index": info.index,
                        "x": info.x,
                        "y": info.y,
                        "width": info.width,
                        "height": info.height,
                        "position": position,
                        "is_primary": info.index == manager.get_primary_monitor_index(),
                        "name": info.device_id,
                        "scale_factor": 1.0,  # MSS captures at physical resolution
                    }
                )

            self.event_manager.emit_log(
                "info",
                f"Retrieved {len(monitors)} monitor(s)",
            )

            return {
                "success": True,
                "monitors": monitors,
                "count": len(monitors),
            }

        except Exception as e:
            print(
                f"[error   ] EXECUTOR: Failed to get monitors: {e}",
                file=sys.stderr,
                flush=True,
            )
            self.event_manager.emit_log("error", f"Failed to get monitors: {e}")
            return {"success": False, "error": str(e)}

    def _handle_segment_screenshot(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle SAM3 segmentation command.

        This uses the qontinui library's SegmentVectorizer with SAM3 to
        segment a screenshot into UI elements.

        Args:
            params: Command parameters:
                - screenshot_base64: Base64 encoded image data
                - min_area: Optional minimum segment area in pixels
                - model: Optional SAM model name

        Returns:
            Dictionary with:
                - success: Whether segmentation succeeded
                - segments: List of segment info with id, bbox, area, image_base64
                - error: Error message (if failed)
        """
        import base64
        import io
        import sys

        print(
            "[info    ] EXECUTOR: _handle_segment_screenshot called",
            file=sys.stderr,
            flush=True,
        )

        try:
            if not QONTINUI_AVAILABLE:
                return {
                    "success": False,
                    "error": "Qontinui library not available",
                }

            # Get screenshot data
            screenshot_base64 = params.get("screenshot_base64", "")
            if not screenshot_base64:
                return {"success": False, "error": "No screenshot_base64 provided"}

            # Remove data URL prefix if present
            if "," in screenshot_base64:
                screenshot_base64 = screenshot_base64.split(",", 1)[1]

            # Decode base64 to image
            try:
                image_bytes = base64.b64decode(screenshot_base64)
            except Exception as e:
                return {"success": False, "error": f"Failed to decode base64: {e}"}

            # Convert to numpy array via PIL
            import numpy as np
            from PIL import Image
            from PIL.Image import Image as PILImage

            pil_image = Image.open(io.BytesIO(image_bytes))
            # Convert to RGB if necessary (SAM expects RGB)
            if pil_image.mode != "RGB":
                pil_image: PILImage = pil_image.convert("RGB")  # type: ignore[no-redef]
            screenshot = np.array(pil_image)

            self.event_manager.emit_log(
                "info",
                f"Segmenting screenshot: {screenshot.shape[1]}x{screenshot.shape[0]} pixels",
            )

            # Try to use SAM3 via SegmentVectorizer
            try:
                from qontinui.rag.segment_vectorizer import HAS_SAM3, SegmentVectorizer

                # Get options
                min_area = params.get("min_area", 100)

                # Create vectorizer (will try to use SAM3)
                vectorizer = SegmentVectorizer()

                # Check if SAM is available
                if not HAS_SAM3:
                    self.event_manager.emit_log(
                        "warning",
                        "SAM3 not available, falling back to grid segmentation. Install sam2 package for better results.",
                    )

                # Run segmentation
                # Run segmentation - vectorize_screenshot returns SegmentVector objects
                segment_vectors = vectorizer.vectorize_screenshot(screenshot)

                # Convert SegmentVector objects to dict format
                segments_raw = [
                    {
                        "id": f"segment_{idx}",
                        "bbox": seg.bbox,
                        "area": seg.area,
                        "image": None,  # Not directly available from SegmentVector
                    }
                    for idx, seg in enumerate(segment_vectors)
                ]

                # Convert to output format
                segments = []
                for i, seg in enumerate(segments_raw):
                    # Get bounding box
                    bbox = seg.get("bbox", [0, 0, 0, 0])
                    if isinstance(bbox, tuple):
                        bbox = list(bbox)

                    # Get area
                    area = seg.get("area", 0)
                    if area < min_area:
                        continue

                    # Get cropped image if available
                    image_base64_out = None
                    if "image" in seg and seg["image"] is not None:
                        cropped = seg["image"]
                        if isinstance(cropped, np.ndarray):
                            # Convert numpy array to base64
                            cropped_pil = Image.fromarray(cropped)
                            buffer = io.BytesIO()
                            cropped_pil.save(buffer, format="PNG", compress_level=6)
                            buffer.seek(0)
                            image_base64_out = base64.b64encode(buffer.getvalue()).decode("utf-8")

                    segments.append(
                        {
                            "id": seg.get("id", f"segment_{i}"),
                            "bbox": bbox,
                            "area": area,
                            "image_base64": image_base64_out,
                        }
                    )

                self.event_manager.emit_log(
                    "info",
                    f"Segmentation complete: {len(segments)} segments found",
                )

                return {
                    "success": True,
                    "segments": segments,
                    "sam_available": HAS_SAM3,
                }

            except ImportError as e:
                self.event_manager.emit_log(
                    "error",
                    f"SegmentVectorizer not available: {e}",
                )
                return {
                    "success": False,
                    "error": f"SegmentVectorizer not available: {e}",
                }

        except Exception as e:
            print(
                f"[error   ] EXECUTOR: Failed to segment screenshot: {e}",
                file=sys.stderr,
                flush=True,
            )

            print(
                f"[error   ] EXECUTOR: Traceback: {traceback.format_exc()}",
                file=sys.stderr,
                flush=True,
            )
            self.event_manager.emit_log("error", f"Failed to segment screenshot: {e}")
            return {"success": False, "error": str(e)}

    def _handle_detect_current_states(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle detect_current_states command for AI Verification Agent.

        This command detects which of the specified states are currently active
        by checking for their identifying images on the screen.

        Args:
            params: Command parameters:
                - state_ids: List of state IDs to check
                - monitor_index: Optional monitor index (0-based)
                - capture_screenshot: Whether to capture and return screenshot

        Returns:
            Dictionary with:
                - success: Whether detection completed
                - detected_states: List of state IDs that are currently active
                - detection_details: Per-state detection results
                - screenshot_base64: Optional screenshot if requested
                - detection_time_ms: Total detection time
        """
        import sys

        print(
            f"[info    ] EXECUTOR: _handle_detect_current_states called with params: {params}",
            file=sys.stderr,
            flush=True,
        )

        try:
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

            # Import verification service
            from services.verification_service import VerificationService

            # Create verification service with current config
            verification_service = VerificationService(
                state_executor=(self.executor_core.state_executor if self.executor_core else None),
                action_executor=(
                    self.executor_core.action_executor if self.executor_core else None
                ),
                config=self.config,
            )

            # Set screenshot directory if capture is requested
            capture_screenshot = params.get("capture_screenshot", False)
            if capture_screenshot:
                dev_logs_dir = RUNNER_PARENT_DIR / ".dev-logs" / "verification"
                verification_service.set_screenshot_directory(dev_logs_dir)

            # Detect states (async method called from sync context)
            import asyncio

            state_ids = params.get("state_ids", [])
            monitor_index = params.get("monitor_index")

            loop = self._get_or_create_async_loop()
            future = asyncio.run_coroutine_threadsafe(
                verification_service.detect_current_states(
                    state_ids=state_ids,
                    monitor_index=monitor_index,
                ),
                loop,
            )
            result = future.result(timeout=120)  # 2 minute timeout

            self.event_manager.emit_log(
                "info",
                f"State detection complete: {len(result.get('detected_states', []))} states detected",
            )

            return result

        except Exception as e:
            print(
                f"[error   ] EXECUTOR: Failed to detect states: {e}",
                file=sys.stderr,
                flush=True,
            )
            print(
                f"[error   ] EXECUTOR: Traceback: {traceback.format_exc()}",
                file=sys.stderr,
                flush=True,
            )
            self.event_manager.emit_log("error", f"Failed to detect states: {e}")
            return {
                "success": False,
                "error": str(e),
                "detected_states": [],
                "detection_details": [],
            }

    def _handle_verify_elements(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle verify_elements command for AI Verification Agent.

        This command verifies the presence of expected elements and absence
        of unexpected elements on the screen.

        Args:
            params: Command parameters:
                - expected_elements: List of image IDs that should be visible
                - unexpected_elements: List of image IDs that should NOT be visible
                - monitor_index: Optional monitor index (0-based)

        Returns:
            Dictionary with:
                - success: Whether verification completed
                - all_expected_found: True if all expected elements were found
                - no_unexpected_found: True if no unexpected elements were found
                - expected_results: Per-element results for expected elements
                - unexpected_results: Per-element results for unexpected elements
                - verification_time_ms: Total verification time
        """
        import sys

        print(
            f"[info    ] EXECUTOR: _handle_verify_elements called with params: {params}",
            file=sys.stderr,
            flush=True,
        )

        try:
            if not QONTINUI_AVAILABLE:
                return {
                    "success": False,
                    "error": "Qontinui library not available",
                    "all_expected_found": False,
                    "no_unexpected_found": True,
                    "expected_results": [],
                    "unexpected_results": [],
                }

            if not self.config:
                return {
                    "success": False,
                    "error": "Configuration not loaded",
                    "all_expected_found": False,
                    "no_unexpected_found": True,
                    "expected_results": [],
                    "unexpected_results": [],
                }

            # Import verification service
            from services.verification_service import VerificationService

            # Create verification service with current config
            verification_service = VerificationService(
                state_executor=(self.executor_core.state_executor if self.executor_core else None),
                action_executor=(
                    self.executor_core.action_executor if self.executor_core else None
                ),
                config=self.config,
            )

            # Verify elements (async method called from sync context)
            import asyncio

            expected_elements = params.get("expected_elements", [])
            unexpected_elements = params.get("unexpected_elements", [])
            monitor_index = params.get("monitor_index")

            loop = self._get_or_create_async_loop()
            future = asyncio.run_coroutine_threadsafe(
                verification_service.verify_elements(
                    expected_elements=expected_elements,
                    unexpected_elements=unexpected_elements,
                    monitor_index=monitor_index,
                ),
                loop,
            )
            result = future.result(timeout=120)  # 2 minute timeout

            self.event_manager.emit_log(
                "info",
                f"Element verification complete: expected_found={result.get('all_expected_found')}, "
                f"no_unexpected={result.get('no_unexpected_found')}",
            )

            return result

        except Exception as e:
            print(
                f"[error   ] EXECUTOR: Failed to verify elements: {e}",
                file=sys.stderr,
                flush=True,
            )
            print(
                f"[error   ] EXECUTOR: Traceback: {traceback.format_exc()}",
                file=sys.stderr,
                flush=True,
            )
            self.event_manager.emit_log("error", f"Failed to verify elements: {e}")
            return {
                "success": False,
                "error": str(e),
                "all_expected_found": False,
                "no_unexpected_found": True,
                "expected_results": [],
                "unexpected_results": [],
            }

    def _handle_verification_capture_screenshot(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle verification_capture_screenshot command for AI Verification Agent.

        This command captures a screenshot specifically for verification documentation.
        It uses the VerificationService to capture and optionally save the screenshot.

        Args:
            params: Command parameters:
                - context: Context string for naming (e.g., state_id)
                - monitor_index: Optional monitor index (0-based)
                - save_to_file: Whether to save to file (default: True)
                - output_directory: Optional output directory path

        Returns:
            Dictionary with:
                - success: Whether capture succeeded
                - screenshot_base64: Base64 encoded PNG data
                - file_path: Path where screenshot was saved (if save_to_file=True)
                - width: Screenshot width in pixels
                - height: Screenshot height in pixels
        """
        import sys

        print(
            f"[info    ] EXECUTOR: _handle_verification_capture_screenshot called with params: {params}",
            file=sys.stderr,
            flush=True,
        )

        try:
            if not QONTINUI_AVAILABLE:
                return {
                    "success": False,
                    "error": "Qontinui library not available",
                }

            # Import verification service
            from services.verification_service import VerificationService

            # Create verification service
            verification_service = VerificationService(
                state_executor=(self.executor_core.state_executor if self.executor_core else None),
                action_executor=(
                    self.executor_core.action_executor if self.executor_core else None
                ),
                config=self.config,
            )

            # Set output directory
            output_directory = params.get("output_directory")
            if output_directory:
                verification_service.set_screenshot_directory(output_directory)
            else:
                dev_logs_dir = RUNNER_PARENT_DIR / ".dev-logs" / "verification" / "screenshots"
                verification_service.set_screenshot_directory(dev_logs_dir)

            # Capture screenshot
            context = params.get("context", "verification")
            monitor_index = params.get("monitor_index")
            save_to_file = params.get("save_to_file", True)

            result = verification_service.capture_screenshot(
                context=context,
                monitor_index=monitor_index,
                save_to_file=save_to_file,
            )

            if result.get("success"):
                self.event_manager.emit_log(
                    "info",
                    f"Verification screenshot captured: {result.get('width')}x{result.get('height')} pixels",
                )
            else:
                self.event_manager.emit_log(
                    "error",
                    f"Failed to capture verification screenshot: {result.get('error')}",
                )

            return result

        except Exception as e:
            print(
                f"[error   ] EXECUTOR: Failed to capture verification screenshot: {e}",
                file=sys.stderr,
                flush=True,
            )
            print(
                f"[error   ] EXECUTOR: Traceback: {traceback.format_exc()}",
                file=sys.stderr,
                flush=True,
            )
            self.event_manager.emit_log("error", f"Failed to capture verification screenshot: {e}")
            return {
                "success": False,
                "error": str(e),
            }

    def _handle_get_flakiness_options(self, params: dict[str, Any]) -> dict[str, Any]:
        """Get flakiness-aware execution options for a transition or template.

        This command is called before executing transitions or matching templates
        to get adjusted execution options based on historical flakiness data.

        The actual flakiness data is stored in the Rust runner's SQLite database.
        This Python handler provides a convenient way to query it and cache
        the options for use during execution.

        Args:
            params: Command parameters:
                - config_id: Configuration ID to load flakiness data for
                - transition_id: Optional transition ID (format: "FromState|ToState")
                - template_id: Optional template/image ID

        Returns:
            Dictionary with:
                - success: Whether the query succeeded
                - options: ExecutionOptions dict with:
                    - retry_count: Number of retry attempts
                    - timeout_multiplier: Multiplier for timeout duration
                    - confidence_threshold: Confidence threshold for matching
                    - use_alternative_path: Whether to use alternative path
                - is_flaky: Whether the item is known to be flaky
        """
        import sys

        config_id = params.get("config_id")
        transition_id = params.get("transition_id")
        template_id = params.get("template_id")

        print(
            f"[info    ] EXECUTOR: _handle_get_flakiness_options: config_id={config_id}, "
            f"transition_id={transition_id}, template_id={template_id}",
            file=sys.stderr,
            flush=True,
        )

        # Default execution options
        default_options = {
            "retry_count": 1,
            "timeout_multiplier": 1.0,
            "confidence_threshold": 0.8,
            "use_alternative_path": False,
        }

        if not config_id:
            return {
                "success": True,
                "options": default_options,
                "is_flaky": False,
                "note": "No config_id provided, returning defaults",
            }

        # Note: The actual flakiness data is queried via Tauri commands from the Rust side.
        # This Python handler is primarily for cases where the Python executor needs to
        # make decisions about execution strategy before calling back to Rust.
        #
        # For most use cases, the frontend or Rust code should call the Tauri command
        # `get_execution_options` directly.
        #
        # Here we return sensible defaults and let the caller know they should
        # query the Rust side for accurate flakiness data.

        self.event_manager.emit_log(
            "debug",
            f"Flakiness options requested for config={config_id}, "
            f"transition={transition_id}, template={template_id}",
        )

        return {
            "success": True,
            "options": default_options,
            "is_flaky": False,
            "note": "Query Rust get_execution_options command for accurate flakiness data",
        }

    def _handle_pattern_find(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle pattern find (best match) command.

        Args:
            params: Command parameters:
                - screenshot: Base64 encoded screenshot or file path
                - template: Base64 encoded template image or file path
                - similarity: Minimum similarity threshold (0.0 to 1.0)
                - search_region: Optional {x, y, width, height}

        Returns:
            Dictionary with match results
        """
        import asyncio

        try:
            service = get_pattern_matching_service()

            screenshot = params.get("screenshot", "")
            template = params.get("template", "")
            similarity = params.get("similarity", 0.8)
            search_region = params.get("search_region")
            invariant = params.get("invariant", False)
            invariant_scales = params.get("invariant_scales")

            if not screenshot:
                return {"success": False, "error": "screenshot is required"}
            if not template:
                return {"success": False, "error": "template is required"}

            # Run async method
            loop = asyncio.new_event_loop()
            asyncio.set_event_loop(loop)
            try:
                result = loop.run_until_complete(
                    service.find(
                        screenshot=screenshot,
                        template=template,
                        similarity=similarity,
                        search_region=search_region,
                        invariant=invariant,
                        invariant_scales=invariant_scales,
                    )
                )
            finally:
                loop.close()

            return {
                "success": result.success,
                "matches": result.matches,
                "search_time_ms": result.search_time_ms,
                "screenshot_width": result.screenshot_width,
                "screenshot_height": result.screenshot_height,
                "template_width": result.template_width,
                "template_height": result.template_height,
                "error": result.error,
            }

        except Exception as e:
            logger.exception(f"Pattern find failed: {e}")
            return {"success": False, "error": str(e)}

    def _handle_pattern_find_all(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle pattern find all command.

        Args:
            params: Command parameters:
                - screenshot: Base64 encoded screenshot or file path
                - template: Base64 encoded template image or file path
                - similarity: Minimum similarity threshold (0.0 to 1.0)
                - search_region: Optional {x, y, width, height}
                - max_matches: Maximum number of matches (default 100)

        Returns:
            Dictionary with all match results
        """
        import asyncio

        try:
            service = get_pattern_matching_service()

            screenshot = params.get("screenshot", "")
            template = params.get("template", "")
            similarity = params.get("similarity", 0.8)
            search_region = params.get("search_region")
            max_matches = params.get("max_matches", 100)
            invariant = params.get("invariant", False)
            invariant_scales = params.get("invariant_scales")

            if not screenshot:
                return {"success": False, "error": "screenshot is required"}
            if not template:
                return {"success": False, "error": "template is required"}

            # Run async method
            loop = asyncio.new_event_loop()
            asyncio.set_event_loop(loop)
            try:
                result = loop.run_until_complete(
                    service.find_all(
                        screenshot=screenshot,
                        template=template,
                        similarity=similarity,
                        search_region=search_region,
                        max_matches=max_matches,
                        invariant=invariant,
                        invariant_scales=invariant_scales,
                    )
                )
            finally:
                loop.close()

            return {
                "success": result.success,
                "matches": result.matches,
                "search_time_ms": result.search_time_ms,
                "screenshot_width": result.screenshot_width,
                "screenshot_height": result.screenshot_height,
                "template_width": result.template_width,
                "template_height": result.template_height,
                "error": result.error,
            }

        except Exception as e:
            logger.exception(f"Pattern find all failed: {e}")
            return {"success": False, "error": str(e)}
