"""
Web Extraction Service for qontinui-runner.

Handles web extraction requests from the Rust bridge using the new extraction architecture.
Coordinates with the qontinui library's ExtractionOrchestrator for unified extraction.
"""

import asyncio
import base64
import json
import logging
import os
import time
import uuid
from pathlib import Path
from typing import Any

import aiohttp
import cv2
import numpy as np
from PIL import Image
from qontinui_schemas.common import utc_now

logger = logging.getLogger(__name__)

# Rust runner API URL for status updates
RUNNER_API_URL = "http://localhost:9876"

# Try to import OCR name generator
try:
    from qontinui.discovery.state_construction.ocr_name_generator import OCRNameGenerator

    HAS_OCR = True
except ImportError:
    HAS_OCR = False
    logger.warning("OCRNameGenerator not available - elements will use default names")

# Import state machine builder for transforming extraction results
try:
    from qontinui.state_management.builders import build_state_machine_from_extraction_result

    HAS_STATE_MACHINE_BUILDER = True
    logger.info("State machine builder imported successfully")
except ImportError as e:
    HAS_STATE_MACHINE_BUILDER = False
    logger.warning(
        f"State machine builder not available - extraction results will not be transformed: {e}"
    )

# Import vision extraction from qontinui library
try:
    from qontinui.extraction.vision import UnifiedVisionExtractor

    HAS_VISION_EXTRACTION = True
except ImportError:
    HAS_VISION_EXTRACTION = False
    logger.warning("UnifiedVisionExtractor not available - vision extraction will be skipped")


class WebExtractionService:
    """
    Service for managing web extraction operations using the new extraction architecture.

    Supports both single-application and multi-application extraction,
    producing ApplicationStateStructure or CompositeStateStructure outputs.
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
        self._orchestrator = None
        self._current_extraction_id: str | None = None
        self._is_running = False
        self._current_task: asyncio.Task | None = (
            None  # Track current extraction task for cancellation
        )

        # Backend integration for status updates
        self._backend_session_id: str | None = None
        self._backend_url: str | None = None
        self._backend_auth_token: str | None = None

        # Store extraction results
        self._extraction_results: dict[str, Any] = {}  # extraction_id -> ExtractionResult
        self._composite_results: dict[str, dict] = {}  # composite_id -> composite structure

        # Vision extraction (lazy initialized)
        self._vision_extractor: UnifiedVisionExtractor | None = None

    async def start_extraction(self, config: dict[str, Any]) -> dict[str, Any]:
        """
        Start extraction for a single application.

        Args:
            config: Extraction configuration with url, viewports, etc.
                - url: Target URL (required for BLACK_BOX)
                - project_path: Path to source code (required for STATIC_ONLY, WHITE_BOX)
                - mode: "black_box", "white_box", or "static_only" (default: "black_box")
                - viewports: List of [width, height] pairs
                - capture_hover_states: bool
                - capture_focus_states: bool
                - capture_scroll_states: bool
                - max_interaction_depth: int
                - framework: Optional framework hint
                - auth_cookies: Optional cookies dict
                - app_id: Optional application identifier
                - app_name: Optional application name

        Returns:
            Dict with success status and extraction_id.
        """
        if self._is_running:
            return {
                "success": False,
                "error": "Extraction already in progress",
            }

        try:
            from qontinui.extraction import (
                ExtractionConfig,
                ExtractionMode,
                ExtractionOrchestrator,
                ExtractionTarget,
                FrameworkType,
            )

            # Parse mode
            mode_str = config.get("mode", "black_box").upper()
            mode = (
                ExtractionMode[mode_str]
                if mode_str in ExtractionMode.__members__
                else ExtractionMode.BLACK_BOX
            )

            # Build extraction target
            target_kwargs = {}

            # Project path for static/white-box analysis
            if "project_path" in config and config["project_path"]:
                target_kwargs["project_path"] = Path(config["project_path"])

            # Runtime access
            # Handle both 'url' (singular) and 'urls' (array from frontend)
            if "urls" in config and config["urls"]:
                # For now, use the first URL from the array
                # Future: support multi-URL extraction
                urls = config["urls"]
                if isinstance(urls, list) and len(urls) > 0:
                    target_kwargs["url"] = urls[0]
                elif isinstance(urls, str):
                    target_kwargs["url"] = urls  # type: ignore[assignment]
            elif "url" in config and config["url"]:
                target_kwargs["url"] = config["url"]
            if "executable_path" in config and config["executable_path"]:
                target_kwargs["executable_path"] = Path(config["executable_path"])
            if "app_id" in config and config["app_id"]:
                target_kwargs["app_id"] = config["app_id"]

            # Framework hint
            if "framework" in config and config["framework"]:
                framework_str = config["framework"].upper()
                if framework_str in FrameworkType.__members__:
                    target_kwargs["framework"] = FrameworkType[framework_str]  # type: ignore[assignment]

            # Authentication
            if "auth_cookies" in config:
                target_kwargs["auth_cookies"] = config["auth_cookies"]
            if "auth_headers" in config:
                target_kwargs["auth_headers"] = config["auth_headers"]
            if "login_url" in config:
                target_kwargs["login_url"] = config["login_url"]

            target = ExtractionTarget(**target_kwargs)  # type: ignore[arg-type]

            # Build extraction config
            # Handle both 'viewports' (plural array) and 'viewport' (singular tuple from frontend)
            viewports_config = config.get("viewports")
            if viewports_config:
                viewports = [tuple(v) for v in viewports_config]
            elif "viewport" in config and config["viewport"]:
                viewports = [tuple(config["viewport"])]
            else:
                viewports = [(1920, 1080)]

            extraction_config = ExtractionConfig(
                target=target,
                mode=mode,
                viewports=viewports,
                capture_hover_states=config.get("capture_hover_states", True),
                capture_focus_states=config.get("capture_focus_states", True),
                capture_scroll_states=config.get("capture_scroll_states", True),
                max_interaction_depth=config.get(
                    "max_depth", config.get("max_interaction_depth", 5)
                ),
                max_pages=config.get("max_pages", 100),
                correlation_threshold=config.get("correlation_threshold", 0.8),
                require_correlation=config.get("require_correlation", True),
                timeout_seconds=config.get("timeout_seconds", 300),
            )

            # Create orchestrator
            self._orchestrator = ExtractionOrchestrator()
            self._current_extraction_id = str(uuid.uuid4())
            self._is_running = True

            # Store backend integration info for status updates
            self._backend_session_id = config.get("session_id")
            self._backend_url = config.get("backend_url")
            self._backend_auth_token = config.get("auth_token")

            # Update backend session to 'running' status
            if self._backend_session_id and self._backend_url:
                await self._update_backend_session(
                    status="running",
                    stats={"pages_extracted": 0, "states_found": 0},
                )

            # Store app metadata
            app_id = config.get("app_id", self._current_extraction_id)
            app_name = config.get("app_name", f"Application {app_id[:8]}")
            self._extraction_results[self._current_extraction_id] = {
                "app_id": app_id,
                "app_name": app_name,
                "config": extraction_config,
                "result": None,
            }

            logger.info(f"Starting extraction {self._current_extraction_id}")

            # Start extraction in background
            self._current_task = asyncio.create_task(self._run_extraction(extraction_config))

            # Add error handler
            def handle_task_exception(t):
                try:
                    exc = t.exception()
                    if exc:
                        logger.error(f"Extraction task exception: {exc}", exc_info=exc)
                except asyncio.CancelledError:
                    logger.info("Extraction task was cancelled")
                finally:
                    # Clear task reference when done
                    if self._current_task is t:
                        self._current_task = None

            self._current_task.add_done_callback(handle_task_exception)

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
            logger.error(f"Failed to start extraction: {e}", exc_info=True)
            return {
                "success": False,
                "error": str(e),
            }

    async def start_multi_app_extraction(self, configs: list[dict[str, Any]]) -> dict[str, Any]:
        """
        Start extraction for multiple applications, creating a CompositeStateStructure.

        Args:
            configs: List of extraction configurations (same format as start_extraction)

        Returns:
            Dict with success status and composite_id.
        """
        if not configs:
            return {"success": False, "error": "No configurations provided"}

        try:
            composite_id = str(uuid.uuid4())
            extraction_ids = []

            # Start all extractions sequentially
            for config in configs:
                result = await self.start_extraction(config)
                if not result["success"]:
                    logger.warning(f"Failed to start extraction for config: {result.get('error')}")
                    continue
                extraction_ids.append(result["extraction_id"])

                # Wait for this extraction to complete before starting next
                while self._is_running:
                    await asyncio.sleep(0.5)

            if not extraction_ids:
                return {
                    "success": False,
                    "error": "All extractions failed to start",
                }

            # Create composite structure
            self._composite_results[composite_id] = {
                "composite_id": composite_id,
                "extraction_ids": extraction_ids,
                "created_at": utc_now().isoformat(),
            }

            return {
                "success": True,
                "composite_id": composite_id,
                "extraction_ids": extraction_ids,
            }

        except Exception as e:
            logger.error(f"Failed to start multi-app extraction: {e}", exc_info=True)
            return {"success": False, "error": str(e)}

    async def add_to_composite(self, composite_id: str, config: dict[str, Any]) -> dict[str, Any]:
        """
        Add another application to an existing composite structure.

        Args:
            composite_id: ID of the composite to add to
            config: Extraction configuration

        Returns:
            Dict with success status and extraction_id.
        """
        if composite_id not in self._composite_results:
            return {"success": False, "error": "Composite not found"}

        try:
            # Start new extraction
            result = await self.start_extraction(config)
            if not result["success"]:
                return result

            # Add to composite
            extraction_id = result["extraction_id"]
            self._composite_results[composite_id]["extraction_ids"].append(extraction_id)

            return {
                "success": True,
                "composite_id": composite_id,
                "extraction_id": extraction_id,
            }

        except Exception as e:
            logger.error(f"Failed to add to composite: {e}", exc_info=True)
            return {"success": False, "error": str(e)}

    async def _run_extraction(self, config) -> None:
        """
        Run the extraction process using ExtractionOrchestrator.

        Args:
            config: ExtractionConfig object
        """
        logger.info("=" * 60)
        logger.info("EXTRACTION STARTED")
        logger.info("=" * 60)
        logger.info(f"Extraction config mode: {config.mode}")
        logger.info(
            f"Extraction config target URL: {config.target.url if config.target else 'None'}"
        )
        logger.info(f"Extraction config viewports: {config.viewports}")
        logger.info(f"Extraction ID: {self._current_extraction_id}")

        if not self._orchestrator:
            logger.error("No orchestrator available!")
            return

        try:
            # Run extraction
            start_extract = time.time()
            logger.info(
                f"[PERF_DEBUG] [{self._current_extraction_id}] Starting orchestrator.extract()"
            )
            result = await self._orchestrator.extract(config)
            duration_extract = time.time() - start_extract
            logger.info(
                f"[PERF_DEBUG] [{self._current_extraction_id}] orchestrator.extract() finished in {duration_extract:.2f}s"
            )
            logger.info("=" * 60)
            logger.info("EXTRACTION RESULT FROM ORCHESTRATOR")
            logger.info("=" * 60)
            logger.info(f"  States count: {len(result.states)}")
            logger.info(f"  Transitions count: {len(result.transitions)}")
            logger.info(f"  Errors: {result.errors}")
            logger.info(f"  Warnings: {result.warnings}")
            logger.info(f"  Framework: {result.framework}")
            logger.info(f"  Mode: {result.mode}")
            if result.runtime_extraction:
                logger.info(f"  Runtime elements: {len(result.runtime_extraction.elements)}")
                logger.info(f"  Runtime states: {len(result.runtime_extraction.states)}")
            else:
                logger.info("  Runtime extraction: None")

            # Store result
            if self._current_extraction_id:
                self._extraction_results[self._current_extraction_id]["result"] = result

                # Use the orchestrator's extraction_id (where screenshots are saved)
                # rather than the runner's ID
                actual_extraction_id = result.extraction_id
                logger.info(f"Orchestrator extraction_id: {actual_extraction_id}")
                logger.info(f"Runner extraction_id: {self._current_extraction_id}")

                # Build state_structure for the frontend
                state_structure = {
                    "type": "application_state_structure",
                    "extraction_id": actual_extraction_id,  # Use orchestrator's ID for screenshot access
                    "framework": result.framework.value,
                    "mode": result.mode.value,
                    "states": [self._serialize_state(s) for s in result.states],
                    "transitions": [self._serialize_transition(t) for t in result.transitions],
                    "metadata": {
                        "started_at": result.started_at.isoformat() if result.started_at else None,
                        "completed_at": (
                            result.completed_at.isoformat() if result.completed_at else None
                        ),
                        "errors": result.errors,
                        "warnings": result.warnings,
                    },
                }

                logger.info(f"Built state_structure with {len(result.states)} states")

                # Serialize states and transitions for backend
                serialized_states = [self._serialize_state(s) for s in result.states]
                serialized_transitions = [self._serialize_transition(t) for t in result.transitions]

                # Serialize elements from runtime extraction
                serialized_elements = []
                if result.runtime_extraction and result.runtime_extraction.elements:
                    serialized_elements = [
                        self._serialize_element(e) for e in result.runtime_extraction.elements
                    ]
                    logger.info(f"Serialized {len(serialized_elements)} elements for backend")

                # Save annotations to backend
                start_save = time.time()
                logger.info(
                    f"[PERF_DEBUG] [{self._current_extraction_id}] Starting _save_annotations_to_backend()"
                )
                await self._save_annotations_to_backend(
                    serialized_states, serialized_transitions, serialized_elements
                )
                duration_save = time.time() - start_save
                logger.info(
                    f"[PERF_DEBUG] [{self._current_extraction_id}] _save_annotations_to_backend() finished in {duration_save:.2f}s"
                )

                # Build and upload the state machine
                # This transforms raw elements into a proper state machine using co-occurrence clustering
                start_build = time.time()
                logger.info(
                    f"[PERF_DEBUG] [{self._current_extraction_id}] Starting _build_and_upload_state_machine()"
                )
                (
                    clustered_states,
                    clustered_transitions,
                ) = await self._build_and_upload_state_machine(serialized_elements)
                duration_build = time.time() - start_build
                logger.info(
                    f"[PERF_DEBUG] [{self._current_extraction_id}] _build_and_upload_state_machine() finished in {duration_build:.2f}s"
                )

                # Update state_structure to use co-occurrence-based states if available
                # This replaces the raw per-page states with properly clustered states
                if clustered_states:
                    logger.info(
                        f"[STATE_STRUCTURE] Updating state_structure with {len(clustered_states)} "
                        f"co-occurrence clustered states (replacing {len(state_structure['states'])} raw states)"
                    )
                    state_structure["states"] = clustered_states
                    state_structure["transitions"] = clustered_transitions

                # Update backend session to completed with stats
                pages_extracted = (
                    result.runtime_extraction.pages_visited if result.runtime_extraction else 1
                )
                # Use clustered state count if available, otherwise fall back to raw states
                final_states_count = (
                    len(clustered_states) if clustered_states else len(result.states)
                )
                await self._update_backend_session(
                    status="completed",
                    stats={
                        "pages_extracted": pages_extracted,
                        "states_found": final_states_count,
                        "transitions_found": (
                            len(clustered_transitions)
                            if clustered_transitions
                            else len(result.transitions)
                        ),
                        "elements_found": (
                            len(result.runtime_extraction.elements)
                            if result.runtime_extraction
                            else 0
                        ),
                        # Include the screenshot extraction_id so frontend knows where to fetch
                        "screenshot_extraction_id": actual_extraction_id,
                    },
                )

                # Emit completion event with state_structure (now containing clustered states)
                await self._emit_event(
                    "extraction_completed",  # Use extraction_completed to match frontend handler
                    {
                        "extraction_id": self._current_extraction_id,
                        "session_id": self._current_extraction_id,
                        "screenshot_extraction_id": actual_extraction_id,  # For screenshot fetching
                        "framework": result.framework.value,
                        "mode": result.mode.value,
                        "state_structure": state_structure,
                        "summary": {
                            "states": final_states_count,
                            "transitions": (
                                len(clustered_transitions)
                                if clustered_transitions
                                else len(result.transitions)
                            ),
                            "errors": len(result.errors),
                            "warnings": len(result.warnings),
                        },
                    },
                )

                logger.info(
                    f"Extraction complete: {final_states_count} clustered states "
                    f"(from {len(result.states)} raw states), "
                    f"{len(clustered_transitions) if clustered_transitions else len(result.transitions)} transitions"
                )

                # Notify Rust that extraction is complete
                await self._notify_rust_extraction_complete(
                    states_found=final_states_count,
                    transitions_found=(
                        len(clustered_transitions)
                        if clustered_transitions
                        else len(result.transitions)
                    ),
                    pages_extracted=(
                        result.runtime_extraction.pages_visited if result.runtime_extraction else 0
                    ),
                    errors=len(result.errors),
                )

        except asyncio.CancelledError:
            # Handle cancellation gracefully
            logger.info(f"Extraction {self._current_extraction_id} was cancelled")
            # Update backend to failed status
            await self._update_backend_session(
                status="failed",
                error_message="Extraction cancelled by user",
            )
            await self._emit_event(
                "extraction_cancelled",
                {
                    "extraction_id": self._current_extraction_id,
                    "message": "Extraction cancelled by user",
                },
            )
            # Notify Rust that extraction was cancelled
            await self._notify_rust_extraction_complete(
                states_found=0,
                transitions_found=0,
                pages_extracted=0,
                errors=0,
            )
            # Re-raise to let asyncio handle the cancellation
            raise

        except Exception as e:
            logger.error(f"Extraction failed: {e}", exc_info=True)
            # Update backend to failed status
            await self._update_backend_session(
                status="failed",
                error_message=str(e),
            )
            await self._emit_event(
                "extraction_error",
                {
                    "extraction_id": self._current_extraction_id,
                    "error": str(e),
                },
            )
            # Notify Rust that extraction failed
            await self._notify_rust_extraction_complete(
                states_found=0,
                transitions_found=0,
                pages_extracted=0,
                errors=1,
            )

        finally:
            self._is_running = False
            self._orchestrator = None

    async def _notify_rust_extraction_complete(
        self,
        states_found: int,
        transitions_found: int,
        pages_extracted: int,
        errors: int,
    ) -> None:
        """
        Notify the Rust runner that extraction has completed.

        This calls the /extraction/complete endpoint to update the status
        that the frontend polls.

        Args:
            states_found: Number of states extracted
            transitions_found: Number of transitions extracted
            pages_extracted: Number of pages visited
            errors: Number of errors encountered
        """
        try:
            async with aiohttp.ClientSession() as session:
                payload = {
                    "states_found": states_found,
                    "transitions_found": transitions_found,
                    "pages_extracted": pages_extracted,
                    "warnings": 0,
                    "errors": errors,
                }
                async with session.post(
                    f"{RUNNER_API_URL}/extraction/complete",
                    json=payload,
                    timeout=aiohttp.ClientTimeout(total=10),
                ) as response:
                    if response.status == 200:
                        logger.info("Notified Rust runner of extraction completion")
                    else:
                        logger.warning(f"Failed to notify Rust runner: {response.status}")
        except Exception as e:
            logger.error(f"Error notifying Rust runner of extraction completion: {e}")

    async def _handle_progress(self, data: dict[str, Any]) -> None:
        """
        Handle progress updates from the extractor.

        Args:
            data: Progress data dict
        """
        event_type = data.get("type", "extraction_progress")

        # Forward to event manager (Rust bridge)
        if self.event_manager:
            self.event_manager.emit_event_wrapper(event_type, data)

        # Forward to websocket (web backend)
        if self.websocket_handler and self.websocket_handler.is_connected:
            await self._send_to_websocket(event_type, data)

    async def _emit_event(self, event_type: str, data: dict[str, Any]) -> None:
        """
        Emit an event to both Rust bridge and websocket.

        Args:
            event_type: Type of event
            data: Event data
        """
        # Use emit_event_wrapper since event_type is a string, not an EventType enum
        if self.event_manager:
            self.event_manager.emit_event_wrapper(event_type, data)

        if self.websocket_handler and self.websocket_handler.is_connected:
            await self._send_to_websocket(event_type, data)

    async def _send_to_websocket(self, event_type: str, data: dict[str, Any]) -> None:
        """
        Send data to websocket handler.

        Args:
            event_type: Type of event
            data: Event data
        """
        try:
            message = {
                "type": event_type,
                "data": data,
                "timestamp": utc_now().isoformat(),
            }
            self.websocket_handler.send_message(json.dumps(message))
        except Exception as e:
            logger.warning(f"Failed to send websocket message: {e}")

    async def stop_extraction(self) -> dict[str, Any]:
        """
        Stop the current extraction process.

        Returns:
            Dict with success status.
        """
        if not self._is_running:
            return {
                "success": False,
                "error": "No extraction in progress",
            }

        try:
            # Proper cancellation implementation:
            # 1. Mark as not running to prevent new operations
            self._is_running = False

            # 2. Cancel the extraction task if it's still running
            if self._current_task and not self._current_task.done():
                logger.info(f"Cancelling extraction task {self._current_extraction_id}")
                self._current_task.cancel()

                # Wait for the task to be cancelled (with timeout)
                try:
                    await asyncio.wait_for(self._current_task, timeout=5.0)
                except asyncio.CancelledError:
                    logger.info("Extraction task cancelled successfully")
                except TimeoutError:
                    logger.warning("Extraction task cancellation timed out")
                except Exception as e:
                    logger.warning(f"Exception during task cancellation: {e}")

            # 3. Clean up orchestrator
            self._orchestrator = None
            self._current_task = None

            # 4. Emit stopped event
            await self._emit_event(
                "extraction_stopped",
                {"extraction_id": self._current_extraction_id},
            )

            return {"success": True}

        except Exception as e:
            logger.error(f"Failed to stop extraction: {e}")
            return {"success": False, "error": str(e)}

    def get_status(self) -> dict[str, Any]:
        """
        Get current extraction status.

        Returns:
            Dict with status information.
        """
        if not self._is_running:
            return {
                "is_running": False,
                "extraction_id": None,
            }

        result = None
        if self._current_extraction_id in self._extraction_results:
            result = self._extraction_results[self._current_extraction_id].get("result")

        return {
            "is_running": True,
            "extraction_id": self._current_extraction_id,
            "stats": (
                {
                    "states_found": len(result.states) if result else 0,
                    "transitions_found": len(result.transitions) if result else 0,
                    "errors": len(result.errors) if result else 0,
                    "warnings": len(result.warnings) if result else 0,
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
            screenshot_id: ID of the screenshot (can be just ID or full path for backwards compat).
            resolution: "thumbnail" or "full".
            extraction_id: Extraction ID (uses current if not specified).

        Returns:
            Dict with base64-encoded image data.
        """
        ext_id = extraction_id or self._current_extraction_id
        if not ext_id:
            return {"success": False, "error": "No extraction specified"}

        # Handle both formats: just ID (e.g., "0001") or full path (backwards compat)
        # If screenshot_id contains path separators, extract just the basename
        if "\\" in screenshot_id or "/" in screenshot_id:
            screenshot_id = os.path.basename(screenshot_id)
        # Remove .png extension if present
        if screenshot_id.endswith(".png"):
            screenshot_id = screenshot_id[:-4]

        screenshots_dir = self.extractions_dir / ext_id / "screenshots"
        filepath = screenshots_dir / f"{screenshot_id}.png"

        if not filepath.exists():
            return {"success": False, "error": "Screenshot not found"}

        try:
            if resolution == "thumbnail":
                # Generate thumbnail
                import io

                from PIL import Image

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

    def export_state_structure(
        self,
        extraction_id: str,
        output_path: str,
        include_screenshots: bool = True,
    ) -> dict[str, Any]:
        """
        Export as ApplicationStateStructure or CompositeStateStructure.

        Args:
            extraction_id: ID of the extraction or composite
            output_path: Path to save output
            include_screenshots: Whether to include screenshot data

        Returns:
            Dict with success status and output path.
        """
        try:
            # Check if it's a composite
            if extraction_id in self._composite_results:
                return self._export_composite(extraction_id, output_path, include_screenshots)

            # Single extraction
            if extraction_id not in self._extraction_results:
                return {"success": False, "error": "Extraction not found"}

            extraction_data = self._extraction_results[extraction_id]
            result = extraction_data.get("result")

            if not result:
                return {"success": False, "error": "Extraction not complete"}

            # Build ApplicationStateStructure-like output
            output = {
                "type": "application_state_structure",
                "app_id": extraction_data["app_id"],
                "app_name": extraction_data["app_name"],
                "extraction_id": extraction_id,
                "framework": result.framework.value,
                "mode": result.mode.value,
                "states": [self._serialize_state(s) for s in result.states],
                "transitions": [self._serialize_transition(t) for t in result.transitions],
                "metadata": {
                    "started_at": result.started_at.isoformat(),
                    "completed_at": (
                        result.completed_at.isoformat() if result.completed_at else None
                    ),
                    "errors": result.errors,
                    "warnings": result.warnings,
                },
            }

            # Save to file
            output_file = Path(output_path)
            output_file.parent.mkdir(parents=True, exist_ok=True)

            with open(output_file, "w") as f:
                json.dump(output, f, indent=2)

            # Copy screenshots if requested
            if include_screenshots:
                screenshots_dir = self.extractions_dir / extraction_id / "screenshots"
                if screenshots_dir.exists():
                    import shutil

                    target_screenshots = output_file.parent / "screenshots"
                    if target_screenshots.exists():
                        shutil.rmtree(target_screenshots)
                    shutil.copytree(screenshots_dir, target_screenshots)

            return {
                "success": True,
                "output_path": str(output_file),
            }

        except Exception as e:
            logger.error(f"Failed to export state structure: {e}", exc_info=True)
            return {"success": False, "error": str(e)}

    def _export_composite(
        self,
        composite_id: str,
        output_path: str,
        include_screenshots: bool,
    ) -> dict[str, Any]:
        """
        Export a composite state structure.

        Args:
            composite_id: ID of the composite
            output_path: Path to save output
            include_screenshots: Whether to include screenshots

        Returns:
            Dict with success status and output path.
        """
        try:
            composite = self._composite_results[composite_id]
            applications = []

            # Collect all application structures
            for ext_id in composite["extraction_ids"]:
                if ext_id not in self._extraction_results:
                    logger.warning(f"Extraction {ext_id} not found in composite")
                    continue

                extraction_data = self._extraction_results[ext_id]
                result = extraction_data.get("result")

                if not result:
                    logger.warning(f"Extraction {ext_id} not complete")
                    continue

                applications.append(
                    {
                        "app_id": extraction_data["app_id"],
                        "app_name": extraction_data["app_name"],
                        "extraction_id": ext_id,
                        "framework": result.framework.value,
                        "mode": result.mode.value,
                        "states": [self._serialize_state(s) for s in result.states],
                        "transitions": [self._serialize_transition(t) for t in result.transitions],
                    }
                )

            # Build composite structure
            output = {
                "type": "composite_state_structure",
                "composite_id": composite_id,
                "applications": applications,
                "created_at": composite["created_at"],
            }

            # Save to file
            output_file = Path(output_path)
            output_file.parent.mkdir(parents=True, exist_ok=True)

            with open(output_file, "w") as f:
                json.dump(output, f, indent=2)

            # Copy screenshots if requested
            if include_screenshots:
                import shutil

                target_screenshots = output_file.parent / "screenshots"
                target_screenshots.mkdir(exist_ok=True)

                for ext_id in composite["extraction_ids"]:
                    screenshots_dir = self.extractions_dir / ext_id / "screenshots"
                    if screenshots_dir.exists():
                        app_screenshots = target_screenshots / ext_id
                        if app_screenshots.exists():
                            shutil.rmtree(app_screenshots)
                        shutil.copytree(screenshots_dir, app_screenshots)

            return {
                "success": True,
                "output_path": str(output_file),
            }

        except Exception as e:
            logger.error(f"Failed to export composite: {e}", exc_info=True)
            return {"success": False, "error": str(e)}

    def export_training_data(
        self,
        extraction_id: str,
        export_format: str,
        output_path: str,
        annotations: dict[str, Any] | None = None,
        include_states: bool = True,
    ) -> dict[str, Any]:
        """
        Export extraction results as training data.

        Args:
            extraction_id: ID of the extraction
            export_format: Export format ("coco", "yolo", "jsonl")
            output_path: Path to save output
            annotations: Optional updated annotations from web backend
            include_states: Whether to include state annotations

        Returns:
            Dict with success status and output path.
        """
        try:
            # Get extraction results
            if extraction_id not in self._extraction_results:
                return {"success": False, "error": "Extraction not found"}

            extraction_data = self._extraction_results[extraction_id]
            result = extraction_data.get("result")

            if not result:
                return {"success": False, "error": "Extraction not complete"}

            # Apply annotation updates if provided
            if annotations:
                # TODO: Implement annotation merging
                logger.warning("Annotation merging not yet implemented")

            # Get runtime extraction data
            runtime_result = result.runtime_extraction
            if not runtime_result:
                return {
                    "success": False,
                    "error": "No runtime extraction data available for training export",
                }

            # Build training data based on format
            output_file = Path(output_path)
            output_file.parent.mkdir(parents=True, exist_ok=True)

            if export_format == "jsonl":
                # Export as JSON Lines format
                with open(output_file, "w") as f:
                    for state in result.states:
                        state_data = self._serialize_state(state)
                        if include_states:
                            state_data["state_info"] = {
                                "confidence": state.confidence,
                                "correlation_score": state.correlation_score,
                            }
                        f.write(json.dumps(state_data) + "\n")

                return {
                    "success": True,
                    "output_path": str(output_file),
                    "format": "jsonl",
                }

            elif export_format in ("coco", "yolo"):
                # For COCO/YOLO formats, we'd need element bounding boxes from runtime extraction
                # This is a placeholder for future implementation
                return {
                    "success": False,
                    "error": f"{export_format.upper()} export not yet implemented in new architecture",
                }

            else:
                return {"success": False, "error": f"Unknown format: {export_format}"}

        except Exception as e:
            logger.error(f"Failed to export training data: {e}", exc_info=True)
            return {"success": False, "error": str(e)}

    def get_composite(self, composite_id: str) -> dict | None:
        """
        Get a composite state structure by ID.

        Args:
            composite_id: ID of the composite

        Returns:
            Composite structure dict or None if not found.
        """
        return self._composite_results.get(composite_id)

    def list_extractions(self) -> dict[str, Any]:
        """
        List all available extractions.

        Returns:
            Dict with list of extractions.
        """
        extractions = []

        # List single extractions
        for ext_id, extraction_data in self._extraction_results.items():
            result = extraction_data.get("result")
            extractions.append(
                {
                    "extraction_id": ext_id,
                    "app_id": extraction_data["app_id"],
                    "app_name": extraction_data["app_name"],
                    "type": "single",
                    "framework": result.framework.value if result else "unknown",
                    "mode": result.mode.value if result else "unknown",
                    "completed": result is not None and result.completed_at is not None,
                    "states": len(result.states) if result else 0,
                    "transitions": len(result.transitions) if result else 0,
                }
            )

        # List composites
        for comp_id, composite in self._composite_results.items():
            extractions.append(
                {
                    "extraction_id": comp_id,
                    "type": "composite",
                    "applications": len(composite["extraction_ids"]),
                    "created_at": composite["created_at"],
                }
            )

        return {
            "success": True,
            "extractions": extractions,
        }

    def _serialize_state(self, state) -> dict[str, Any]:
        """
        Serialize a CorrelatedState to dict.

        Args:
            state: CorrelatedState object

        Returns:
            Serialized state dict.
        """
        # Serialize bbox if available
        bbox_dict = None
        bbox = getattr(state, "bounding_box", None)
        if bbox:
            if hasattr(bbox, "to_dict"):
                bbox_dict = bbox.to_dict()
            elif hasattr(bbox, "x"):
                bbox_dict = {
                    "x": bbox.x,
                    "y": bbox.y,
                    "width": bbox.width,
                    "height": bbox.height,
                }

        # Also check metadata for bbox (set by orchestrator from ExtractedState)
        metadata = state.metadata if hasattr(state, "metadata") else {}
        if bbox_dict is None and isinstance(metadata, dict) and "bbox" in metadata:
            bbox_dict = metadata["bbox"]

        result = {
            "id": state.id,
            "name": state.name,
            "confidence": state.confidence,
            "component_name": state.component_name,
            "route_path": state.route_path,
            "state_variables": state.state_variables,
            "source_file": state.source_file,
            "line_number": state.line_number,
            "runtime_state_id": state.runtime_state_id,
            "screenshot_id": state.screenshot_id,
            "url": state.url,
            "visible_elements": state.visible_elements,
            "correlation_method": state.correlation_method,
            "correlation_score": state.correlation_score,
            "metadata": metadata,
        }

        # Include bbox at top level for easy access
        if bbox_dict:
            result["bbox"] = bbox_dict

        return result

    def _serialize_transition(self, transition) -> dict[str, Any]:
        """
        Serialize an InferredTransition to dict.

        Args:
            transition: InferredTransition object

        Returns:
            Serialized transition dict.
        """
        return {
            "id": transition.id,
            "from_state_id": transition.from_state_id,
            "to_state_id": transition.to_state_id,
            "trigger_type": transition.trigger_type,
            "event_handler": transition.event_handler,
            "source_location": transition.source_location,
            "runtime_transition_id": transition.runtime_transition_id,
            "target_element": transition.target_element,
            "confidence": transition.confidence,
            "metadata": transition.metadata,
        }

    def _serialize_element(self, element) -> dict[str, Any]:
        """
        Serialize an ExtractedElement to dict for the backend API.

        Args:
            element: ExtractedElement object

        Returns:
            Serialized element dict in ElementAnnotation format.
        """
        # Get bbox dict
        bbox_dict = {"x": 0, "y": 0, "width": 0, "height": 0}
        bbox = getattr(element, "bbox", None)
        if bbox:
            if hasattr(bbox, "to_dict"):
                bbox_dict = bbox.to_dict()
            elif hasattr(bbox, "x"):
                bbox_dict = {
                    "x": int(bbox.x),
                    "y": int(bbox.y),
                    "width": int(bbox.width),
                    "height": int(bbox.height),
                }

        # Get element type as string
        element_type = getattr(element, "element_type", None)
        if element_type:
            if hasattr(element_type, "value"):
                element_type = element_type.value
            else:
                element_type = str(element_type)
        else:
            element_type = "unknown"

        return {
            "id": element.id,
            "name": None,  # Will be populated by _apply_ocr_to_elements
            "element_type": element_type,
            "bbox": bbox_dict,
            "text": getattr(element, "text_content", None),
            "selector": getattr(element, "selector", None),
            "confidence": getattr(element, "confidence", 1.0),
            "extraction_category": getattr(element, "extraction_category", ""),
        }

    def _sanitize_text_for_name(self, text: str) -> str:
        """
        Sanitize text to create a valid element name.

        Converts text to lowercase, replaces spaces and special characters
        with underscores, and removes non-alphanumeric characters.

        Args:
            text: Raw text to sanitize

        Returns:
            Sanitized text suitable for use as an element name
        """
        import re

        if not text:
            return ""

        # Convert to lowercase
        sanitized = text.lower().strip()

        # Replace common separators with underscores
        sanitized = re.sub(r"[\s\-/\\.,;:!?()]+", "_", sanitized)

        # Remove any remaining non-alphanumeric characters (except underscores)
        sanitized = re.sub(r"[^a-z0-9_]", "", sanitized)

        # Collapse multiple underscores into single underscore
        sanitized = re.sub(r"_+", "_", sanitized)

        # Remove leading/trailing underscores
        sanitized = sanitized.strip("_")

        # Truncate to reasonable length (50 chars max)
        if len(sanitized) > 50:
            sanitized = sanitized[:50].rstrip("_")

        return sanitized

    def _apply_ocr_to_elements(
        self,
        elements: list[dict[str, Any]],
        screenshot_path: Path,
    ) -> list[dict[str, Any]]:
        """
        Apply OCR to elements to generate meaningful names from their visual content.

        Args:
            elements: List of serialized element dicts
            screenshot_path: Path to the screenshot image

        Returns:
            Updated elements with OCR-based names
        """
        if not elements:
            return elements

        # First, try to set names from existing text content (DOM-extracted)
        # This works even without OCR
        logger.info(f"Processing {len(elements)} elements for naming")
        for element in elements:
            text = element.get("text")
            current_name = element.get("name")
            logger.debug(
                f"Element {element.get('id')}: name={current_name}, text={text[:50] if text else None}"
            )
            if not current_name and text:
                # Sanitize the text to create a valid name
                sanitized = self._sanitize_text_for_name(text)
                logger.debug(f"  Sanitized text: {sanitized}")
                if sanitized and len(sanitized) >= 2:
                    element["name"] = sanitized
                    logger.debug(f"  Set name to: {sanitized}")

        # If OCR is not available, we're done
        if not HAS_OCR:
            named_count = sum(1 for e in elements if e.get("name"))
            logger.info(
                f"Set names from DOM text for {len(elements)} elements: "
                f"{named_count} named (OCR not available)"
            )
            return elements

        if not screenshot_path.exists():
            logger.warning(f"Screenshot not found for OCR: {screenshot_path}")
            named_count = sum(1 for e in elements if e.get("name"))
            logger.info(
                f"Set names from DOM text for {len(elements)} elements: "
                f"{named_count} named (screenshot not found)"
            )
            return elements

        try:
            # Initialize OCR generator (will auto-select best available engine)
            ocr_generator = OCRNameGenerator(engine="auto")

            # Load screenshot as numpy array for OpenCV processing
            pil_image = Image.open(screenshot_path)
            screenshot = np.array(pil_image)

            # Convert RGB to BGR for OpenCV (if color image)
            if len(screenshot.shape) == 3 and screenshot.shape[2] == 3:
                screenshot = cv2.cvtColor(screenshot, cv2.COLOR_RGB2BGR)

            img_height, img_width = screenshot.shape[:2]

            for element in elements:
                bbox = element.get("bbox", {})
                x = int(bbox.get("x", 0))
                y = int(bbox.get("y", 0))
                width = int(bbox.get("width", 0))
                height = int(bbox.get("height", 0))

                # Skip invalid bboxes
                if width <= 0 or height <= 0:
                    continue

                # Clamp to image bounds
                x = max(0, min(x, img_width - 1))
                y = max(0, min(y, img_height - 1))
                x2 = max(0, min(x + width, img_width))
                y2 = max(0, min(y + height, img_height))

                # Skip if region is too small
                if x2 - x < 5 or y2 - y < 5:
                    continue

                # Crop element region
                element_image = screenshot[y:y2, x:x2]

                # First, try to extract raw OCR text
                raw_ocr_text = ocr_generator._extract_text(element_image)

                # Determine the best name for this element
                element_name = None

                if raw_ocr_text and raw_ocr_text.strip():
                    # Use OCR text to generate a sanitized name
                    sanitized_name = ocr_generator._sanitize_text(raw_ocr_text)
                    if sanitized_name and len(sanitized_name) >= 2:
                        element_name = sanitized_name
                        # Update text field with raw OCR if it was empty
                        if not element.get("text"):
                            element["text"] = raw_ocr_text.strip()

                # If no OCR text, try using existing text content from DOM
                if not element_name and element.get("text"):
                    sanitized_name = ocr_generator._sanitize_text(element["text"])
                    if sanitized_name and len(sanitized_name) >= 2:
                        element_name = sanitized_name

                # Set the name if we found something meaningful
                if element_name:
                    element["name"] = element_name
                    logger.debug(f"Element {element['id']} named: {element_name}")

            # Log summary of naming results
            named_count = sum(1 for e in elements if e.get("name"))
            logger.info(
                f"Applied OCR naming to {len(elements)} elements: "
                f"{named_count} named, {len(elements) - named_count} unnamed"
            )

        except Exception as e:
            logger.warning(f"OCR processing failed: {e}", exc_info=True)
            # Return elements unchanged on error

        return elements

    async def _run_vision_extraction(
        self,
        screenshot_path: Path,
        screenshot_id: str,
        source_url: str,
    ) -> dict[str, Any] | None:
        """
        Run vision extraction (Edge, SAM3, OCR) on a screenshot using the qontinui library.

        Args:
            screenshot_path: Path to the screenshot image
            screenshot_id: Unique ID for this screenshot
            source_url: Source URL of the page

        Returns:
            Vision extraction results dict, or None if extraction failed
        """
        logger.debug(
            f"[VISION_DEBUG] _run_vision_extraction called, HAS_VISION_EXTRACTION={HAS_VISION_EXTRACTION}"
        )
        start_vision = time.time()
        logger.info(
            f"[PERF_DEBUG] [{screenshot_id}] Starting _run_vision_extraction for {source_url}"
        )
        if not HAS_VISION_EXTRACTION:
            logger.debug("[VISION_DEBUG] Vision extraction not available, returning None")
            logger.debug("Vision extraction not available")
            return None

        if not screenshot_path.exists():
            logger.debug(f"[VISION_DEBUG] Screenshot not found: {screenshot_path}")
            logger.warning(f"Screenshot not found for vision extraction: {screenshot_path}")
            return None

        try:
            logger.debug("[VISION_DEBUG] Screenshot exists, initializing extractor...")
            # Lazy initialize vision extractor
            if self._vision_extractor is None:
                logger.debug("[VISION_DEBUG] Creating UnifiedVisionExtractor...")
                self._vision_extractor = UnifiedVisionExtractor()
                logger.debug("[VISION_DEBUG] UnifiedVisionExtractor created successfully")

            # Run vision extraction
            logger.debug(f"[VISION_DEBUG] Running extract() on {screenshot_path}")
            logger.info(f"Running vision extraction on {screenshot_path}")
            result = await self._vision_extractor.extract(
                screenshot_path=screenshot_path,
                screenshot_id=screenshot_id,
                source_url=source_url,
            )

            # Convert result to serializable dict
            techniques_run: list[str] = []
            edge_results: list[dict[str, Any]] = []
            sam3_results: list[dict[str, Any]] = []
            ocr_results: list[dict[str, Any]] = []
            merged_candidates: list[dict[str, Any]] = []
            vision_data: dict[str, Any] = {
                "extraction_id": result.extraction_id,
                "duration_ms": result.duration_ms,
                "techniques_run": techniques_run,
                "edge_results": edge_results,
                "sam3_results": sam3_results,
                "ocr_results": ocr_results,
                "merged_candidates": merged_candidates,
                "edge_overlay": result.edge_overlay_image,
                "sam3_overlay": result.sam3_mask_image,
                "ocr_overlay": result.ocr_overlay_image,
            }

            # Serialize edge detection results
            if result.edge_detection_results:
                techniques_run.append("edge")
                for edge in result.edge_detection_results:
                    edge_results.append(
                        {
                            "id": edge.id,
                            "bbox": {
                                "x": edge.bbox.x,
                                "y": edge.bbox.y,
                                "width": edge.bbox.width,
                                "height": edge.bbox.height,
                            },
                            "confidence": edge.confidence,
                            "contour_area": edge.contour_area,
                            "contour_perimeter": edge.contour_perimeter,
                            "vertex_count": edge.vertex_count,
                            "aspect_ratio": edge.aspect_ratio,
                        }
                    )

            # Serialize SAM3 results
            if result.sam3_segments:
                techniques_run.append("sam3")
                for seg in result.sam3_segments:
                    sam3_results.append(
                        {
                            "id": seg.id,
                            "bbox": {
                                "x": seg.bbox.x,
                                "y": seg.bbox.y,
                                "width": seg.bbox.width,
                                "height": seg.bbox.height,
                            },
                            "stability_score": seg.stability_score,
                            "predicted_iou": seg.predicted_iou,
                            "mask_area": seg.mask_area,
                            "confidence": seg.confidence,
                        }
                    )

            # Serialize OCR results
            if result.ocr_results:
                techniques_run.append("ocr")
                for ocr in result.ocr_results:
                    ocr_results.append(
                        {
                            "id": ocr.id,
                            "bbox": {
                                "x": ocr.bbox.x,
                                "y": ocr.bbox.y,
                                "width": ocr.bbox.width,
                                "height": ocr.bbox.height,
                            },
                            "text": ocr.text,
                            "confidence": ocr.confidence,
                            "language": ocr.language,
                        }
                    )

            # Serialize merged candidates
            if result.candidates:
                for candidate in result.candidates:
                    merged_candidates.append(
                        {
                            "id": candidate.id,
                            "bbox": {
                                "x": candidate.bbox.x,
                                "y": candidate.bbox.y,
                                "width": candidate.bbox.width,
                                "height": candidate.bbox.height,
                            },
                            "confidence": candidate.confidence,
                            "category": candidate.category,
                            "text": candidate.text,
                            "detection_technique": candidate.detection_technique,
                            "is_clickable": candidate.is_clickable,
                        }
                    )

            logger.debug(
                f"[VISION_DEBUG] Vision extraction complete: {len(vision_data['edge_results'])} edges, "
                f"{len(vision_data['sam3_results'])} segments, "
                f"{len(vision_data['ocr_results'])} text regions, "
                f"{len(vision_data['merged_candidates'])} merged candidates"
            )
            logger.info(
                f"Vision extraction complete: {len(vision_data['edge_results'])} edges, "
                f"{len(vision_data['sam3_results'])} segments, "
                f"{len(vision_data['ocr_results'])} text regions, "
                f"{len(vision_data['merged_candidates'])} merged candidates"
            )
            duration_vision = time.time() - start_vision
            logger.info(
                f"[PERF_DEBUG] [{screenshot_id}] _run_vision_extraction finished in {duration_vision:.2f}s"
            )
            return vision_data

        except Exception as e:
            logger.warning(f"Vision extraction failed: {e}", exc_info=True)
            return None

    async def _build_and_upload_state_machine(
        self,
        serialized_elements: list[dict[str, Any]],
    ) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
        """
        Build the state machine from extraction results and upload to backend.

        Uses image matching to find elements across screenshots, then groups
        elements by co-occurrence (which screenshots they appear on together).

        The algorithm:
        1. Load screenshots from disk
        2. For each element, crop its image region from the source screenshot
        3. Search for that image on ALL other screenshots using template matching
        4. Record which screenshots contain each image
        5. Group images by co-occurrence (same set of screenshots = same state)

        Args:
            serialized_elements: List of serialized element dicts (unused, kept for API compat)

        Returns:
            Tuple of (states_config, transitions_config) - the co-occurrence-based state machine
        """
        if not self._backend_session_id or not self._backend_url:
            logger.debug("No backend session configured, skipping state machine upload")
            return [], []

        import httpx

        try:
            extraction_id = self._current_extraction_id
            if not extraction_id or extraction_id not in self._extraction_results:
                logger.warning("No extraction results available for state machine building")
                return [], []

            result = self._extraction_results[extraction_id].get("result")
            if not result:
                logger.warning("No extraction result available")
                return [], []

            # Get the screenshots directory
            actual_extraction_id = result.extraction_id
            screenshots_dir = self.extractions_dir / actual_extraction_id / "screenshots"

            logger.info(f"Building state machine with image matching from {screenshots_dir}")
            logger.debug(f"Screenshots dir exists: {screenshots_dir.exists()}")
            logger.debug(f"HAS_STATE_MACHINE_BUILDER: {HAS_STATE_MACHINE_BUILDER}")

            # Use the new image-matching state machine builder from qontinui library
            if HAS_STATE_MACHINE_BUILDER:
                runtime_states_count = (
                    len(result.runtime_extraction.states) if result.runtime_extraction else 0
                )
                logger.info(
                    f"[STATE_MACHINE] Building state machine with {runtime_states_count} runtime states"
                )

                states_config, transitions_config = build_state_machine_from_extraction_result(
                    extraction_result=result,
                    screenshots_dir=screenshots_dir,
                    similarity_threshold=0.8,
                )
                logger.info(
                    f"[STATE_MACHINE] Image matching returned {len(states_config)} states, {len(transitions_config)} transitions"
                )
                for i, state in enumerate(states_config):
                    images_count = len(state.get("stateImages", []))
                    screens_found = state.get("screensFound", [])
                    logger.debug(
                        f"[STATE_MACHINE]   State {i}: images={images_count}, screensFound={screens_found}"
                    )
            else:
                # Fallback: Create a single state with all elements (old behavior)
                logger.warning(
                    "[STATE_MACHINE] State machine builder not available - falling back to single state"
                )
                state_images = []
                for elem in serialized_elements:
                    elem_id = elem.get("id", "")
                    elem_bbox = elem.get("bbox", {"x": 0, "y": 0, "width": 100, "height": 30})
                    elem_name = (
                        elem.get("name")
                        or elem.get("text")
                        or elem.get("element_type")
                        or "Element"
                    )
                    if len(elem_name) > 30:
                        elem_name = elem_name[:27] + "..."

                    state_images.append(
                        {
                            "id": f"stateimage-{elem_id}",
                            "name": elem_name,
                            "patterns": [
                                {
                                    "id": f"pattern-{elem_id}",
                                    "name": elem_name,
                                    "searchRegions": [elem_bbox],
                                    "fixed": False,
                                }
                            ],
                            "shared": False,
                            "searchRegions": [elem_bbox],
                            "extractionCategory": elem.get("extraction_category", ""),
                        }
                    )

                states_config = [
                    {
                        "id": "state-page-elements",
                        "name": "Page Elements",
                        "description": "Interactive elements extracted from the page",
                        "stateImages": state_images,
                        "regions": [],
                        "locations": [],
                        "strings": [],
                        "position": {"x": 0, "y": 0},
                        "initial": True,
                        "isFinal": False,
                    }
                ]
                transitions_config = []

            logger.info(
                f"Built state machine: {len(states_config)} states, "
                f"{len(transitions_config)} transitions"
            )

            # Upload to backend
            url = f"{self._backend_url}/api/v1/extractions/{self._backend_session_id}/state-machine"
            payload = {
                "states": states_config,
                "transitions": transitions_config,
            }

            headers = {"Content-Type": "application/json"}
            if self._backend_auth_token:
                headers["Authorization"] = f"Bearer {self._backend_auth_token}"

            async with httpx.AsyncClient() as client:
                response = await client.put(url, json=payload, headers=headers, timeout=30.0)
                if response.status_code == 200:
                    logger.info(
                        f"Successfully uploaded state machine: "
                        f"{len(states_config)} states, {len(transitions_config)} transitions"
                    )
                else:
                    logger.warning(
                        f"Failed to upload state machine: {response.status_code} - {response.text}"
                    )

            # Return the built state machine for use in the state_structure event
            return states_config, transitions_config

        except Exception as e:
            logger.warning(f"Failed to build/upload state machine: {e}", exc_info=True)
            return [], []

    async def _update_backend_session(
        self,
        status: str,
        stats: dict[str, Any] | None = None,
        error_message: str | None = None,
    ) -> None:
        """
        Update the backend extraction session with new status/stats.

        Args:
            status: Session status ('running', 'completed', 'failed')
            stats: Optional stats dict with pages_extracted, states_found, etc.
            error_message: Optional error message for failed status
        """
        if not self._backend_session_id or not self._backend_url:
            logger.debug("No backend session configured, skipping update")
            return

        import httpx

        try:
            url = f"{self._backend_url}/api/v1/extractions/{self._backend_session_id}"
            payload: dict[str, Any] = {"status": status}

            if stats:
                payload["stats"] = stats
            if error_message:
                payload["error_message"] = error_message

            # Build headers with auth token if available
            headers = {"Content-Type": "application/json"}
            if self._backend_auth_token:
                headers["Authorization"] = f"Bearer {self._backend_auth_token}"

            logger.info(f"Updating backend session {self._backend_session_id} to status={status}")

            async with httpx.AsyncClient() as client:
                response = await client.patch(url, json=payload, headers=headers, timeout=30.0)
                if response.status_code == 200:
                    logger.info(f"Successfully updated backend session to {status}")
                else:
                    logger.warning(
                        f"Failed to update backend session: {response.status_code} - {response.text}"
                    )

        except Exception as e:
            logger.warning(f"Failed to update backend session: {e}")

    def _convert_to_state_annotation(self, state: dict[str, Any], index: int) -> dict[str, Any]:
        """
        Convert a CorrelatedState dict to the StateAnnotation format expected by the backend.

        Args:
            state: Serialized CorrelatedState dict
            index: Index for generating default position

        Returns:
            Dict in StateAnnotation format
        """
        # Try to get bbox from various sources in the state data
        bbox = None

        # 1. Check metadata for bbox (stored by state builder)
        metadata = state.get("metadata", {})
        if isinstance(metadata, dict):
            if "bbox" in metadata:
                bbox_data = metadata["bbox"]
                # Handle tuple format (x, y, w, h)
                if isinstance(bbox_data, (list, tuple)) and len(bbox_data) >= 4:
                    bbox = {
                        "x": int(bbox_data[0]),
                        "y": int(bbox_data[1]),
                        "width": int(bbox_data[2]),
                        "height": int(bbox_data[3]),
                    }
                # Handle dict format
                elif isinstance(bbox_data, dict):
                    bbox = {
                        "x": int(bbox_data.get("x", 0)),
                        "y": int(bbox_data.get("y", 0)),
                        "width": int(bbox_data.get("width", 200)),
                        "height": int(bbox_data.get("height", 80)),
                    }

        # 2. Check for bounding_box field directly on state (CorrelatedState model)
        if bbox is None and "bounding_box" in state:
            bbox_data = state["bounding_box"]
            if isinstance(bbox_data, dict):
                bbox = {
                    "x": int(bbox_data.get("x", 0)),
                    "y": int(bbox_data.get("y", 0)),
                    "width": int(bbox_data.get("width", 200)),
                    "height": int(bbox_data.get("height", 80)),
                }

        # 3. Check for bbox field directly on state (ExtractedState model)
        if bbox is None and "bbox" in state:
            bbox_data = state["bbox"]
            if isinstance(bbox_data, dict):
                bbox = {
                    "x": int(bbox_data.get("x", 0)),
                    "y": int(bbox_data.get("y", 0)),
                    "width": int(bbox_data.get("width", 200)),
                    "height": int(bbox_data.get("height", 80)),
                }

        # 4. Fallback to placeholder values
        if bbox is None:
            bbox = {
                "x": 0,
                "y": index * 100,  # Stack states vertically for visualization
                "width": 200,
                "height": 80,
            }

        return {
            "id": state.get("id", str(uuid.uuid4())),
            "name": state.get("name", f"state_{index}"),
            "bbox": bbox,
            "state_type": state.get("correlation_method", state.get("state_type", "extracted")),
            "element_ids": state.get("visible_elements", state.get("element_ids", [])) or [],
        }

    async def _save_annotations_to_backend(
        self,
        states: list[dict[str, Any]],
        transitions: list[dict[str, Any]],
        elements: list[dict[str, Any]] | None = None,
    ) -> None:
        """
        Save extraction annotations to the backend.

        Args:
            states: List of serialized state dicts (CorrelatedState format)
            transitions: List of serialized transition dicts
            elements: List of serialized element dicts (ExtractedElement format)
        """
        logger.info(
            f"[ANNOTATION_DEBUG] _save_annotations_to_backend called with {len(states)} states, {len(transitions)} transitions"
        )

        if not self._backend_session_id or not self._backend_url:
            logger.debug("No backend session configured, skipping annotation save")
            return

        import httpx

        elements = elements or []
        logger.info(f"[ANNOTATION_DEBUG] elements count: {len(elements)}")

        # Group states by their screenshot_id (each screenshot is a separate annotation)
        states_by_screenshot: dict[str, list[dict[str, Any]]] = {}
        for state in states:
            # Use the state's actual screenshot_id, fallback to URL-based grouping
            screenshot_id = state.get("screenshot_id") or state.get("url", "unknown")
            if screenshot_id not in states_by_screenshot:
                states_by_screenshot[screenshot_id] = []
            states_by_screenshot[screenshot_id].append(state)

        # Group elements by their screenshot (elements belong to states which have screenshot_ids)
        # We need to associate elements with states via element_ids
        # For now, we group all elements with the first screenshot since runtime extraction
        # captures elements per page visit
        elements_by_screenshot: dict[str, list[dict[str, Any]]] = {}

        # Build a mapping of element_id -> element
        element_map = {elem["id"]: elem for elem in elements}
        logger.debug(f"Element map has {len(element_map)} elements")

        # Assign elements to screenshots based on which states reference them
        for screenshot_id, screenshot_states in states_by_screenshot.items():
            if screenshot_id not in elements_by_screenshot:
                elements_by_screenshot[screenshot_id] = []

            # Collect all element IDs referenced by states in this screenshot
            for state in screenshot_states:
                element_ids = state.get("visible_elements", []) or []
                logger.debug(
                    f"State {state.get('name', state.get('id', 'unknown'))} "
                    f"has {len(element_ids)} element IDs"
                )
                for elem_id in element_ids:
                    if elem_id in element_map:
                        elem = element_map[elem_id]
                        # Avoid duplicates
                        if elem not in elements_by_screenshot[screenshot_id]:
                            elements_by_screenshot[screenshot_id].append(elem)
                    else:
                        logger.debug(f"Element {elem_id} not found in element_map")

        # Build headers with auth token if available
        headers = {"Content-Type": "application/json"}
        if self._backend_auth_token:
            headers["Authorization"] = f"Bearer {self._backend_auth_token}"

        # Get extraction ID for screenshot lookup
        extraction_id = None
        if self._current_extraction_id and self._current_extraction_id in self._extraction_results:
            extraction_data = self._extraction_results[self._current_extraction_id]
            result = extraction_data.get("result")
            if result:
                extraction_id = result.extraction_id

        try:
            async with httpx.AsyncClient() as client:
                for screenshot_id, screenshot_states in states_by_screenshot.items():
                    # Get the URL from the first state in this screenshot group
                    source_url = screenshot_states[0].get("url", "unknown")

                    # Convert CorrelatedState format to StateAnnotation format
                    converted_states = [
                        self._convert_to_state_annotation(state, idx)
                        for idx, state in enumerate(screenshot_states)
                    ]

                    # Get elements for this screenshot
                    screenshot_elements = elements_by_screenshot.get(screenshot_id, [])

                    # Build screenshot path for OCR and vision extraction
                    screenshot_path = None
                    logger.debug(
                        f"[VISION_DEBUG] extraction_id={extraction_id}, screenshot_id={screenshot_id}"
                    )
                    if extraction_id:
                        screenshot_path = (
                            self.extractions_dir
                            / extraction_id
                            / "screenshots"
                            / f"{screenshot_id}.png"
                        )
                        try:
                            exists = screenshot_path.exists()
                            logger.debug(
                                f"[VISION_DEBUG] screenshot_path={screenshot_path}, exists={exists}"
                            )
                        except Exception as e:
                            logger.debug(f"[VISION_DEBUG] exists() check failed: {e}")

                    # Apply OCR to elements if we can find the screenshot
                    if screenshot_elements and screenshot_path:
                        screenshot_elements = self._apply_ocr_to_elements(
                            screenshot_elements, screenshot_path
                        )

                    # Run vision extraction on the screenshot
                    vision_results = None
                    logger.debug(
                        f"[VISION_DEBUG] HAS_VISION_EXTRACTION={HAS_VISION_EXTRACTION}, screenshot_path set={screenshot_path is not None}"
                    )
                    if screenshot_path:
                        logger.debug("[VISION_DEBUG] Calling _run_vision_extraction...")
                        vision_results = await self._run_vision_extraction(
                            screenshot_path=screenshot_path,
                            screenshot_id=screenshot_id,
                            source_url=source_url,
                        )
                        logger.debug(
                            f"[VISION_DEBUG] Vision results: {vision_results is not None}, techniques={vision_results.get('techniques_run') if vision_results else None}"
                        )

                    annotation_data = {
                        "screenshot_id": screenshot_id,
                        "source_url": source_url,
                        "elements": screenshot_elements,
                        "states": converted_states,
                        "vision_results": vision_results,
                    }

                    url = f"{self._backend_url}/api/v1/extractions/{self._backend_session_id}/annotations"

                    logger.debug(f"Sending annotation data: {annotation_data}")

                    response = await client.put(
                        url, json=annotation_data, headers=headers, timeout=30.0
                    )
                    if response.status_code == 200:
                        logger.info(
                            f"Saved annotation for {source_url} (screenshot: {screenshot_id}) "
                            f"with {len(converted_states)} states and {len(screenshot_elements)} elements"
                        )
                    else:
                        logger.warning(
                            f"Failed to save annotation: {response.status_code} - {response.text}"
                        )

        except Exception as e:
            logger.warning(f"Failed to save annotations to backend: {e}")
