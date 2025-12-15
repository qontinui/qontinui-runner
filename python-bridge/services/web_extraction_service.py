"""
Web Extraction Service for qontinui-runner.

Handles web extraction requests from the Rust bridge using the new extraction architecture.
Coordinates with the qontinui library's ExtractionOrchestrator for unified extraction.
"""

import asyncio
import base64
import json
import logging
import uuid
from datetime import datetime
from pathlib import Path
from typing import Any

logger = logging.getLogger(__name__)


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

        # Store extraction results
        self._extraction_results: dict[str, Any] = {}  # extraction_id -> ExtractionResult
        self._composite_results: dict[str, dict] = {}  # composite_id -> composite structure

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
                max_interaction_depth=config.get("max_interaction_depth", 3),
                correlation_threshold=config.get("correlation_threshold", 0.8),
                require_correlation=config.get("require_correlation", True),
                timeout_seconds=config.get("timeout_seconds", 300),
            )

            # Create orchestrator
            self._orchestrator = ExtractionOrchestrator()
            self._current_extraction_id = str(uuid.uuid4())
            self._is_running = True

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
                "created_at": datetime.now().isoformat(),
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
            logger.info("Calling orchestrator.extract()...")
            result = await self._orchestrator.extract(config)
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

                # Build state_structure for the frontend
                state_structure = {
                    "type": "application_state_structure",
                    "extraction_id": self._current_extraction_id,
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

                # Emit completion event with state_structure
                await self._emit_event(
                    "extraction_completed",  # Use extraction_completed to match frontend handler
                    {
                        "extraction_id": self._current_extraction_id,
                        "session_id": self._current_extraction_id,
                        "framework": result.framework.value,
                        "mode": result.mode.value,
                        "state_structure": state_structure,
                        "summary": {
                            "states": len(result.states),
                            "transitions": len(result.transitions),
                            "errors": len(result.errors),
                            "warnings": len(result.warnings),
                        },
                    },
                )

                logger.info(
                    f"Extraction complete: {len(result.states)} states, "
                    f"{len(result.transitions)} transitions"
                )

        except asyncio.CancelledError:
            # Handle cancellation gracefully
            logger.info(f"Extraction {self._current_extraction_id} was cancelled")
            await self._emit_event(
                "extraction_cancelled",
                {
                    "extraction_id": self._current_extraction_id,
                    "message": "Extraction cancelled by user",
                },
            )
            # Re-raise to let asyncio handle the cancellation
            raise

        except Exception as e:
            logger.error(f"Extraction failed: {e}", exc_info=True)
            await self._emit_event(
                "extraction_error",
                {
                    "extraction_id": self._current_extraction_id,
                    "error": str(e),
                },
            )

        finally:
            self._is_running = False
            self._orchestrator = None

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
                "timestamp": datetime.now().isoformat(),
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
        format: str,
        output_path: str,
        annotations: dict[str, Any] | None = None,
        include_states: bool = True,
    ) -> dict[str, Any]:
        """
        Export extraction results as training data.

        Args:
            extraction_id: ID of the extraction
            format: Export format ("coco", "yolo", "jsonl")
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

            if format == "jsonl":
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

            elif format in ("coco", "yolo"):
                # For COCO/YOLO formats, we'd need element bounding boxes from runtime extraction
                # This is a placeholder for future implementation
                return {
                    "success": False,
                    "error": f"{format.upper()} export not yet implemented in new architecture",
                }

            else:
                return {"success": False, "error": f"Unknown format: {format}"}

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
        return {
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
            "metadata": state.metadata,
        }

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
