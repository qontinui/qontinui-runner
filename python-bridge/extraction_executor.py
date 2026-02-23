#!/usr/bin/env python3
"""
Extraction Executor - Lightweight Python Process for Web Extraction

This module provides a separate Python process for handling web extraction operations.
It runs independently of the main qontinui_executor.py, allowing extraction to run
in parallel with GUI automation workflows.

The extraction executor only handles extraction-related commands:
- start_web_extraction
- stop_web_extraction
- get_extraction_status
- get_extraction_screenshot
- export_training_data
- export_state_structure
- list_extractions
- run_vision_extraction

This separation enables concurrent execution because:
1. Extraction uses Playwright (browser automation) - no HAL required
2. GUI automation uses HAL (screen capture/input) - exclusive access required
3. Both can run simultaneously without contention
"""

import asyncio
import json
import logging
import os
import sys
import tempfile
import threading
import time
import traceback
from datetime import datetime
from pathlib import Path
from typing import Any

# CRITICAL: Configure logging to use stderr FIRST before any other imports
logging.basicConfig(
    stream=sys.stderr, level=logging.DEBUG, format="%(asctime)s [%(levelname)s] %(message)s"
)

# CRITICAL: Check for --disable-console-logging flag BEFORE any imports
if "--disable-console-logging" in sys.argv:
    os.environ["QONTINUI_DISABLE_CONSOLE_LOGGING"] = "1"
    sys.argv.remove("--disable-console-logging")

# IMMEDIATE debug logging to verify executor is being invoked
debug_log_path = os.path.join(tempfile.gettempdir(), "extraction_executor_startup.log")
try:
    with open(debug_log_path, "a", encoding="utf-8") as f:
        timestamp = datetime.now().strftime("%Y-%m-%d %H:%M:%S.%f")[:-3]
        f.write(f"[{timestamp}] EXTRACTION EXECUTOR STARTED - extraction_executor.py is running\n")
        f.write(
            f"[{timestamp}] Console logging disabled: {os.getenv('QONTINUI_DISABLE_CONSOLE_LOGGING') == '1'}\n"
        )
except Exception:
    pass

# CRITICAL: Send READY signal IMMEDIATELY to prevent timeout
sys.stdout.write(
    json.dumps(
        {
            "type": "ready",
            "data": {"message": "Extraction executor starting", "extraction_only": True},
        }
    )
    + "\n"
)
sys.stdout.flush()

# Set up logging
logger = logging.getLogger(__name__)

# Add qontinui library src directory to path
qontinui_src_path = Path(__file__).parent.parent.parent / "qontinui" / "src"
sys.path.insert(0, str(qontinui_src_path))

# Import our specialized modules
from event_manager import EventManager, EventType  # noqa: E402
from services.vision_extraction_service import VisionExtractionService  # noqa: E402
from services.web_extraction_service import WebExtractionService  # noqa: E402
from websocket_handler import WebSocketHandler  # noqa: E402


class ExtractionExecutor:
    """
    Lightweight executor for extraction operations only.

    This executor handles web extraction using Playwright (browser automation)
    and vision extraction using computer vision techniques. It does not
    depend on HAL or GUI automation, allowing it to run in parallel with
    the main qontinui_executor.

    Responsibilities:
    - Handle extraction commands from Rust bridge
    - Coordinate WebExtractionService for Playwright-based extraction
    - Coordinate VisionExtractionService for vision-based extraction
    - Emit events to notify Rust of extraction progress/completion
    """

    def __init__(self) -> None:
        """Initialize extraction executor and required modules."""
        # Initialize EventManager for emitting events
        self.event_manager = EventManager()

        # Initialize WebSocketHandler for streaming to web backend
        self.websocket_handler = WebSocketHandler(
            emit_log_fn=self.event_manager.emit_log, on_command_fn=self._handle_websocket_command
        )

        # Extraction services (lazy initialized)
        self._web_extraction_service: WebExtractionService | None = None
        self._vision_extraction_service: VisionExtractionService | None = None

        # Async event loop for extraction (created on demand)
        self._async_loop: asyncio.AbstractEventLoop | None = None
        self._async_thread: threading.Thread | None = None

        logger.info("Extraction executor initialized")

    def _handle_websocket_command(self, command: dict[str, Any]) -> None:
        """Handle commands received via WebSocket."""
        logger.warning("WebSocket commands not supported in extraction executor")

    def _get_web_extraction_service(self) -> WebExtractionService:
        """Get or create the web extraction service."""
        if self._web_extraction_service is None:
            self._web_extraction_service = WebExtractionService(
                event_manager=self.event_manager,
                websocket_handler=self.websocket_handler,
            )
        return self._web_extraction_service

    def _get_vision_extraction_service(self) -> VisionExtractionService:
        """Get or create the vision extraction service."""
        if self._vision_extraction_service is None:
            self._vision_extraction_service = VisionExtractionService(
                event_manager=self.event_manager,
            )
        return self._vision_extraction_service

    def _get_or_create_async_loop(self):
        """Get or create a dedicated event loop for async operations."""
        import asyncio

        if self._async_thread is None or not self._async_thread.is_alive():
            self._async_loop = asyncio.new_event_loop()
            self._async_thread = threading.Thread(
                target=self._start_async_loop,
                daemon=True,
            )
            self._async_thread.start()
            # Wait for loop to be ready
            timeout = 5
            start_time = time.time()
            while self._async_loop is None and time.time() - start_time < timeout:
                time.sleep(0.05)
        return self._async_loop

    def _start_async_loop(self):
        """Start the async event loop in its thread."""
        import asyncio

        asyncio.set_event_loop(self._async_loop)
        self._async_loop.run_forever()

    def handle_command(self, command: dict) -> dict[str, Any]:
        """
        Handle a command from the Rust bridge.

        Args:
            command: Command dict with 'command' and 'params' keys

        Returns:
            Result dict with 'success' and optional 'error' keys
        """
        cmd_name = command.get("command", "unknown")
        params = command.get("params", {})

        try:
            # Route to appropriate handler
            if cmd_name == "ping":
                return self._handle_ping()
            elif cmd_name == "stop":
                return self._handle_stop()
            elif cmd_name == "start_web_extraction":
                return self._handle_start_web_extraction(params)
            elif cmd_name == "stop_web_extraction":
                return self._handle_stop_web_extraction()
            elif cmd_name == "get_extraction_status":
                return self._handle_get_extraction_status()
            elif cmd_name == "get_extraction_screenshot":
                return self._handle_get_extraction_screenshot(params)
            elif cmd_name == "export_training_data":
                return self._handle_export_training_data(params)
            elif cmd_name == "export_state_structure":
                return self._handle_export_state_structure(params)
            elif cmd_name == "list_extractions":
                return self._handle_list_extractions()
            elif cmd_name == "run_vision_extraction":
                return self._handle_run_vision_extraction(params)
            elif cmd_name == "ws_configure":
                return self._handle_ws_configure(params)
            elif cmd_name == "ws_connect":
                return self._handle_ws_connect()
            elif cmd_name == "ws_disconnect":
                return self._handle_ws_disconnect()
            else:
                return {
                    "success": False,
                    "error": f"Unknown command: {cmd_name}. This executor only handles extraction commands.",
                }

        except Exception as e:
            logger.error(f"Error handling command {cmd_name}: {e}")
            logger.error(traceback.format_exc())
            return {
                "success": False,
                "error": str(e),
            }

    def _handle_ping(self) -> dict[str, Any]:
        """Handle ping command."""
        # Send pong response via stdout (not through normal response)
        pong_response = {"type": "pong", "timestamp": time.time()}
        with self.event_manager._output_lock:
            sys.stdout.write(json.dumps(pong_response) + "\n")
            sys.stdout.flush()
        return {"success": True}

    def _handle_stop(self) -> dict[str, Any]:
        """Handle stop command - gracefully shut down."""
        logger.info("Extraction executor received stop command, shutting down")
        # Stop any running extraction
        if self._web_extraction_service and self._web_extraction_service._is_running:
            import asyncio

            loop = self._get_or_create_async_loop()
            asyncio.run_coroutine_threadsafe(
                self._web_extraction_service.stop_extraction(), loop
            ).result(timeout=10)
        return {"success": True}

    def _handle_start_web_extraction(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle start_web_extraction command."""
        import asyncio

        try:
            service = self._get_web_extraction_service()
            loop = self._get_or_create_async_loop()

            # Get config from params
            config = params.get("config", params)

            # Schedule async extraction on background loop
            future = asyncio.run_coroutine_threadsafe(service.start_extraction(config), loop)
            result = future.result(timeout=60)  # 1 minute timeout for startup

            if result.get("success"):
                self.event_manager.emit_event(
                    EventType.EXTRACTION_STARTED,
                    {"extraction_id": result.get("extraction_id"), "config": config},
                )

            return result

        except Exception as e:
            logger.error(f"Failed to start web extraction: {e}")
            logger.error(traceback.format_exc())
            self.event_manager.emit_log("error", f"Failed to start web extraction: {e}")
            return {"success": False, "error": str(e)}

    def _handle_stop_web_extraction(self) -> dict[str, Any]:
        """Handle stop_web_extraction command."""
        import asyncio

        try:
            service = self._get_web_extraction_service()
            loop = self._get_or_create_async_loop()

            future = asyncio.run_coroutine_threadsafe(service.stop_extraction(), loop)
            result = future.result(timeout=30)

            return result

        except Exception as e:
            logger.error(f"Failed to stop web extraction: {e}")
            self.event_manager.emit_log("error", f"Failed to stop web extraction: {e}")
            return {"success": False, "error": str(e)}

    def _handle_get_extraction_status(self) -> dict[str, Any]:
        """Handle get_extraction_status command."""
        try:
            service = self._get_web_extraction_service()
            status = service.get_status()
            return {"success": True, **status}
        except Exception as e:
            logger.error(f"Failed to get extraction status: {e}")
            return {"success": False, "error": str(e)}

    def _handle_get_extraction_screenshot(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle get_extraction_screenshot command."""
        try:
            service = self._get_web_extraction_service()
            screenshot_id = params.get("screenshot_id")
            resolution = params.get("resolution", "thumbnail")
            extraction_id = params.get("extraction_id")

            if not screenshot_id:
                return {"success": False, "error": "screenshot_id is required"}

            result = service.get_screenshot(
                screenshot_id=screenshot_id,
                resolution=resolution,
                extraction_id=extraction_id,
            )
            return result

        except Exception as e:
            logger.error(f"Failed to get extraction screenshot: {e}")
            return {"success": False, "error": str(e)}

    def _handle_export_training_data(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle export_training_data command."""
        try:
            service = self._get_web_extraction_service()
            extraction_id = params.get("extraction_id")
            export_format = params.get("format", "jsonl")
            output_path = params.get("output_path")
            annotations = params.get("annotations")
            include_states = params.get("include_states", True)

            if not extraction_id:
                return {"success": False, "error": "extraction_id is required"}
            if not output_path:
                return {"success": False, "error": "output_path is required"}

            result = service.export_training_data(
                extraction_id=extraction_id,
                export_format=export_format,
                output_path=output_path,
                annotations=annotations,
                include_states=include_states,
            )
            return result

        except Exception as e:
            logger.error(f"Failed to export training data: {e}")
            return {"success": False, "error": str(e)}

    def _handle_export_state_structure(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle export_state_structure command."""
        try:
            service = self._get_web_extraction_service()
            extraction_id = params.get("extraction_id")
            output_path = params.get("output_path")
            include_screenshots = params.get("include_screenshots", True)

            if not extraction_id:
                return {"success": False, "error": "extraction_id is required"}
            if not output_path:
                return {"success": False, "error": "output_path is required"}

            result = service.export_state_structure(
                extraction_id=extraction_id,
                output_path=output_path,
                include_screenshots=include_screenshots,
            )
            return result

        except Exception as e:
            logger.error(f"Failed to export state structure: {e}")
            return {"success": False, "error": str(e)}

    def _handle_list_extractions(self) -> dict[str, Any]:
        """Handle list_extractions command."""
        try:
            service = self._get_web_extraction_service()
            return service.list_extractions()
        except Exception as e:
            logger.error(f"Failed to list extractions: {e}")
            return {"success": False, "error": str(e)}

    def _handle_run_vision_extraction(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle run_vision_extraction command."""
        try:
            service = self._get_vision_extraction_service()
            config = params.get("config", params)
            result = service.extract(config)

            if result.get("success"):
                self.event_manager.emit_event(
                    EventType.EXTRACTION_STARTED,
                    {"extraction_id": result.get("extraction_id"), "technique": "vision"},
                )

            return result

        except Exception as e:
            logger.error(f"Failed to run vision extraction: {e}")
            self.event_manager.emit_log("error", f"Failed to run vision extraction: {e}")
            return {"success": False, "error": str(e)}

    def _handle_ws_configure(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle ws_configure command."""
        try:
            enabled = params.get("enabled", True)
            api_url = params.get("api_url", "")
            jwt_token = params.get("jwt_token", "")
            project_id = params.get("project_id")
            runner_name = params.get("runner_name")

            self.websocket_handler.configure(
                enabled=enabled,
                api_url=api_url,
                token=jwt_token,
                project_id=project_id,
                runner_name=runner_name,
            )
            return {"success": True}
        except Exception as e:
            logger.error(f"Failed to configure WebSocket: {e}")
            return {"success": False, "error": str(e)}

    def _handle_ws_connect(self) -> dict[str, Any]:
        """Handle ws_connect command - runs in background to avoid blocking pings."""
        import threading

        def _connect_in_background():
            try:
                success = self.websocket_handler.connect()
                if not success:
                    logger.error("WebSocket background connect failed")
            except Exception as e:
                logger.error(f"Failed to connect WebSocket: {e}")

        thread = threading.Thread(target=_connect_in_background, daemon=True)
        thread.start()
        return {"success": True}

    def _handle_ws_disconnect(self) -> dict[str, Any]:
        """Handle ws_disconnect command."""
        try:
            self.websocket_handler.disconnect()
            return {"success": True}
        except Exception as e:
            logger.error(f"Failed to disconnect WebSocket: {e}")
            return {"success": False, "error": str(e)}


def main():
    """Main entry point for the extraction executor."""
    executor = ExtractionExecutor()

    executor.event_manager.emit_log(
        "info", "Extraction executor main loop started, waiting for commands"
    )

    # Read commands from stdin
    for line in sys.stdin:
        try:
            command = json.loads(line.strip())
            cmd_name = command.get("command", "unknown")

            # Don't log high-frequency commands
            if cmd_name not in ("ping", "status"):
                executor.event_manager.emit_log("info", f"Received command: {cmd_name}")

            if command.get("type") == "command":
                result = executor.handle_command(command)
                # Wrap response in expected format
                response = {
                    "type": "response",
                    "id": command.get("id"),
                    "success": result.get("success", False),
                    "data": {k: v for k, v in result.items() if k not in ("success", "error")},
                    "error": result.get("error"),
                }

                with executor.event_manager._output_lock:
                    sys.stdout.write(json.dumps(response) + "\n")
                    sys.stdout.flush()

        except json.JSONDecodeError as e:
            logger.error(f"Invalid JSON: {e}")
        except Exception as e:
            logger.error(f"Error handling command: {e}")
            logger.error(traceback.format_exc())


if __name__ == "__main__":
    main()
