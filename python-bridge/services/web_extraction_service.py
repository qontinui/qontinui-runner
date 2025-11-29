"""
Web Extraction Service for qontinui-runner.

Handles web extraction requests from the Rust bridge, coordinating
with the qontinui library's extraction module.
"""

import asyncio
import base64
import json
import logging
from pathlib import Path
from typing import Any, Callable
from datetime import datetime

logger = logging.getLogger(__name__)


class WebExtractionService:
    """
    Service for managing web extraction operations.

    Coordinates with qontinui's extraction module and streams
    progress updates back to the Rust bridge and web backend.
    """

    def __init__(
        self,
        event_manager=None,
        websocket_handler=None,
    ):
        """
        Initialize the web extraction service.

        Args:
            event_manager: EventManager for emitting events to Rust bridge.
            websocket_handler: WebSocketHandler for streaming to web backend.
        """
        self.event_manager = event_manager
        self.websocket_handler = websocket_handler

        # Storage directory for extractions
        self.extractions_dir = Path.home() / ".qontinui" / "extraction"
        self.extractions_dir.mkdir(parents=True, exist_ok=True)

        # Current extraction state
        self._extractor = None
        self._current_extraction_id: str | None = None
        self._is_running = False
        self._extraction_results: dict[str, Any] = {}  # extraction_id -> results

    async def start_extraction(self, config: dict[str, Any]) -> dict[str, Any]:
        """
        Start a web extraction process.

        Args:
            config: Extraction configuration with urls, viewports, etc.

        Returns:
            Dict with success status and extraction_id.
        """
        if self._is_running:
            return {
                "success": False,
                "error": "Extraction already in progress",
            }

        try:
            # Import qontinui extraction module
            from qontinui.extraction.web import WebExtractor, ExtractionConfig

            # Build config
            extraction_config = ExtractionConfig(
                urls=config.get("urls", []),
                viewports=[tuple(v) for v in config.get("viewports", [(1920, 1080)])],
                capture_hover_states=config.get("capture_hover_states", True),
                capture_focus_states=config.get("capture_focus_states", True),
                capture_scroll_states=config.get("capture_scroll_states", True),
                max_depth=config.get("max_depth", 5),
                max_pages=config.get("max_pages", 100),
                auth_cookies=config.get("auth_cookies", {}),
            )

            # Create extractor
            self._extractor = WebExtractor(extraction_config)
            self._current_extraction_id = self._extractor.extraction_id
            self._is_running = True

            import sys

            logger.info(f"Starting extraction {self._current_extraction_id}")
            print(
                f"[info    ] WEB_EXTRACTION: Creating background task for extraction {self._current_extraction_id}",
                file=sys.stderr,
                flush=True,
            )

            # Start extraction in background with error handler
            task = asyncio.create_task(self._run_extraction())

            # Add callback to log any unhandled exceptions
            def handle_task_exception(t):
                try:
                    exc = t.exception()
                    if exc:
                        print(
                            f"[error   ] WEB_EXTRACTION: Task exception: {exc}",
                            file=sys.stderr,
                            flush=True,
                        )
                except asyncio.CancelledError:
                    print(
                        f"[info    ] WEB_EXTRACTION: Task was cancelled",
                        file=sys.stderr,
                        flush=True,
                    )

            task.add_done_callback(handle_task_exception)
            print(f"[info    ] WEB_EXTRACTION: Task created: {task}", file=sys.stderr, flush=True)

            return {
                "success": True,
                "extraction_id": self._current_extraction_id,
            }

        except ImportError as e:
            logger.error(f"Failed to import extraction module: {e}")
            return {
                "success": False,
                "error": f"Extraction module not available: {e}",
            }
        except Exception as e:
            logger.error(f"Failed to start extraction: {e}")
            return {
                "success": False,
                "error": str(e),
            }

    async def _run_extraction(self) -> None:
        """Run the extraction process."""
        import sys

        print(f"[info    ] WEB_EXTRACTION: _run_extraction started", file=sys.stderr, flush=True)

        if not self._extractor:
            print(
                f"[error   ] WEB_EXTRACTION: No extractor available!", file=sys.stderr, flush=True
            )
            return

        try:
            print(
                f"[info    ] WEB_EXTRACTION: Calling extractor.start()...",
                file=sys.stderr,
                flush=True,
            )
            result = await self._extractor.start(progress_callback=self._handle_progress)
            print(
                f"[info    ] WEB_EXTRACTION: extractor.start() completed",
                file=sys.stderr,
                flush=True,
            )

            # Store results
            self._extraction_results[result.extraction_id] = result

            # Emit completion event
            await self._emit_event(
                "extraction_complete",
                {
                    "extraction_id": result.extraction_id,
                    "summary": {
                        "total_pages": len(result.page_extractions),
                        "total_states": len(result.states),
                        "total_elements": len(result.elements),
                        "total_transitions": len(result.transitions),
                    },
                },
            )

        except Exception as e:
            import traceback

            print(f"[error   ] WEB_EXTRACTION: Extraction failed: {e}", file=sys.stderr, flush=True)
            print(
                f"[error   ] WEB_EXTRACTION: Traceback: {traceback.format_exc()}",
                file=sys.stderr,
                flush=True,
            )
            logger.error(f"Extraction failed: {e}")
            await self._emit_event(
                "extraction_error",
                {
                    "extraction_id": self._current_extraction_id,
                    "error": str(e),
                },
            )

        finally:
            print(
                f"[info    ] WEB_EXTRACTION: _run_extraction finished", file=sys.stderr, flush=True
            )
            self._is_running = False
            self._extractor = None

    async def _handle_progress(self, data: dict[str, Any]) -> None:
        """Handle progress updates from the extractor."""
        event_type = data.get("type", "extraction_progress")

        # Forward to event manager (Rust bridge)
        # Use emit_event_wrapper since event_type is a string, not an EventType enum
        if self.event_manager:
            self.event_manager.emit_event_wrapper(event_type, data)

        # Forward to websocket (web backend)
        if self.websocket_handler and self.websocket_handler.is_connected:
            await self._send_to_websocket(event_type, data)

    async def _emit_event(self, event_type: str, data: dict[str, Any]) -> None:
        """Emit an event to both Rust bridge and websocket."""
        # Use emit_event_wrapper since event_type is a string, not an EventType enum
        if self.event_manager:
            self.event_manager.emit_event_wrapper(event_type, data)

        if self.websocket_handler and self.websocket_handler.is_connected:
            await self._send_to_websocket(event_type, data)

    async def _send_to_websocket(self, event_type: str, data: dict[str, Any]) -> None:
        """Send data to websocket handler."""
        try:
            message = {
                "type": event_type,
                "data": data,
                "timestamp": datetime.now().isoformat(),
            }
            self.websocket_handler.send_message(json.dumps(message))
        except Exception as e:
            logger.warning(f"Failed to send websocket message: {e}")

    async def stop_extraction(self) -> dict[str, Any]:
        """Stop the current extraction process."""
        if not self._is_running or not self._extractor:
            return {
                "success": False,
                "error": "No extraction in progress",
            }

        try:
            await self._extractor.stop()
            return {"success": True}
        except Exception as e:
            logger.error(f"Failed to stop extraction: {e}")
            return {"success": False, "error": str(e)}

    def get_status(self) -> dict[str, Any]:
        """Get current extraction status."""
        if not self._is_running:
            return {
                "is_running": False,
                "extraction_id": None,
            }

        result = self._extractor.get_results() if self._extractor else None

        return {
            "is_running": True,
            "extraction_id": self._current_extraction_id,
            "stats": (
                {
                    "pages_extracted": len(result.page_extractions) if result else 0,
                    "states_found": len(result.states) if result else 0,
                    "elements_found": len(result.elements) if result else 0,
                    "transitions_found": len(result.transitions) if result else 0,
                }
                if result
                else None
            ),
        }

    def get_screenshot(
        self,
        screenshot_id: str,
        resolution: str = "thumbnail",
        extraction_id: str | None = None,
    ) -> dict[str, Any]:
        """
        Get a screenshot from an extraction.

        Args:
            screenshot_id: ID of the screenshot.
            resolution: "thumbnail" or "full".
            extraction_id: Extraction ID (uses current if not specified).

        Returns:
            Dict with base64-encoded image data.
        """
        ext_id = extraction_id or self._current_extraction_id
        if not ext_id:
            return {"success": False, "error": "No extraction specified"}

        screenshots_dir = self.extractions_dir / ext_id / "screenshots"
        filepath = screenshots_dir / f"{screenshot_id}.png"

        if not filepath.exists():
            return {"success": False, "error": "Screenshot not found"}

        try:
            if resolution == "thumbnail":
                # Generate thumbnail
                from PIL import Image
                import io

                with Image.open(filepath) as img:
                    img.thumbnail((400, 300))
                    buffer = io.BytesIO()
                    img.save(buffer, format="PNG")
                    data = base64.b64encode(buffer.getvalue()).decode()
            else:
                # Full resolution
                with open(filepath, "rb") as f:
                    data = base64.b64encode(f.read()).decode()

            return {
                "success": True,
                "screenshot_id": screenshot_id,
                "resolution": resolution,
                "data": data,
            }

        except Exception as e:
            logger.error(f"Failed to get screenshot: {e}")
            return {"success": False, "error": str(e)}

    def export_training_data(
        self,
        extraction_id: str,
        format: str,
        output_path: str,
        annotations: dict[str, Any] | None = None,
        include_states: bool = True,
    ) -> dict[str, Any]:
        """
        Export extraction results as training data.

        Args:
            extraction_id: ID of the extraction.
            format: Export format ("coco", "yolo", "jsonl").
            output_path: Path to save output.
            annotations: Optional updated annotations from web backend.
            include_states: Whether to include state annotations.

        Returns:
            Dict with success status and output path.
        """
        try:
            # Get extraction results
            if extraction_id not in self._extraction_results:
                return {"success": False, "error": "Extraction not found"}

            result = self._extraction_results[extraction_id]

            # Apply annotation updates if provided
            if annotations:
                result = self._apply_annotations(result, annotations)

            # Get screenshots directory
            screenshots_dir = self.extractions_dir / extraction_id / "screenshots"

            # Import and use exporter
            from qontinui.extraction.web.output import TrainingDataExporter

            exporter = TrainingDataExporter()
            output_path = Path(output_path)

            if format == "coco":
                result_path = exporter.export_coco(
                    result, output_path, screenshots_dir, include_states
                )
            elif format == "yolo":
                result_path = exporter.export_yolo(
                    result, output_path, screenshots_dir, include_states
                )
            elif format == "jsonl":
                result_path = exporter.export_jsonl(
                    result, output_path / "annotations.jsonl", include_states
                )
            else:
                return {"success": False, "error": f"Unknown format: {format}"}

            return {
                "success": True,
                "output_path": str(result_path),
            }

        except Exception as e:
            logger.error(f"Failed to export training data: {e}")
            return {"success": False, "error": str(e)}

    def export_state_structure(
        self,
        extraction_id: str,
        output_path: str,
        include_screenshots: bool = True,
    ) -> dict[str, Any]:
        """
        Export extraction results as Qontinui state structure.

        Args:
            extraction_id: ID of the extraction.
            output_path: Path to save output.
            include_screenshots: Whether to copy screenshots.

        Returns:
            Dict with success status and output path.
        """
        try:
            if extraction_id not in self._extraction_results:
                return {"success": False, "error": "Extraction not found"}

            result = self._extraction_results[extraction_id]
            screenshots_dir = self.extractions_dir / extraction_id / "screenshots"

            from qontinui.extraction.web.output import StateStructureExporter

            exporter = StateStructureExporter(include_screenshots=include_screenshots)
            result_path = exporter.export(
                result,
                Path(output_path),
                screenshots_dir if include_screenshots else None,
            )

            return {
                "success": True,
                "output_path": str(result_path),
            }

        except Exception as e:
            logger.error(f"Failed to export state structure: {e}")
            return {"success": False, "error": str(e)}

    def list_extractions(self) -> dict[str, Any]:
        """List all available extractions."""
        extractions = []

        for ext_dir in self.extractions_dir.iterdir():
            if ext_dir.is_dir():
                metadata_file = ext_dir / "metadata.json"
                if metadata_file.exists():
                    try:
                        with open(metadata_file) as f:
                            metadata = json.load(f)
                        extractions.append(
                            {
                                "extraction_id": ext_dir.name,
                                **metadata,
                            }
                        )
                    except Exception:
                        extractions.append(
                            {
                                "extraction_id": ext_dir.name,
                            }
                        )
                else:
                    extractions.append(
                        {
                            "extraction_id": ext_dir.name,
                        }
                    )

        return {
            "success": True,
            "extractions": extractions,
        }

    def _apply_annotations(
        self,
        result,
        annotations: dict[str, Any],
    ):
        """Apply annotation updates from web backend to extraction results."""
        # This would update bounding boxes, element classifications, etc.
        # based on user edits in the web UI
        # For now, just return the original result
        # TODO: Implement annotation merging
        return result
