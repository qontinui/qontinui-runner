#!/usr/bin/env python3
"""
Qontinui Executor - Main Entry Point

This module composes specialized modules following the Single Responsibility Principle:
- event_manager.py: Event handling and emission
- websocket_handler.py: WebSocket client communication
- capture_manager.py: Screenshot and video capture
- training_export.py: Training data export coordination
- executor_core.py: Core configuration loading and initialization
- gui_automation.py: GUI interaction and action execution

The executor acts as a facade, coordinating these modules while maintaining
the same stdin/stdout protocol for Rust bridge communication.
"""

import json
import sys
import logging
import os
import tempfile
import threading
import time
import traceback
from datetime import datetime
from pathlib import Path
from typing import Any, Dict, List, Optional

# CRITICAL: Configure logging to use stderr FIRST before any other imports
logging.basicConfig(
    stream=sys.stderr, level=logging.DEBUG, format="%(asctime)s [%(levelname)s] %(message)s"
)

# CRITICAL: Check for --disable-console-logging flag BEFORE any imports
if "--disable-console-logging" in sys.argv:
    os.environ["QONTINUI_DISABLE_CONSOLE_LOGGING"] = "1"
    sys.argv.remove("--disable-console-logging")

# IMMEDIATE debug logging to verify executor is being invoked
debug_log_path = os.path.join(tempfile.gettempdir(), "qontinui_executor_startup.log")
try:
    with open(debug_log_path, "a", encoding="utf-8") as f:
        timestamp = datetime.now().strftime("%Y-%m-%d %H:%M:%S.%f")[:-3]
        f.write(f"[{timestamp}] EXECUTOR STARTED - qontinui_executor.py is running\n")
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
            "data": {"message": "Python executor starting", "library_available": None},
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

logger.debug(
    f"Qontinui source path added to sys.path: {qontinui_src_path} (exists: {qontinui_src_path.exists()})"
)

# CRITICAL: Import local python-bridge modules BEFORE qontinui library
from event_translator import EventTranslator
from execution_tree import ExecutionTree, ExecutionNode
from action_definitions import get_action_definition
from services.unified_data_collector import UnifiedDataCollector
from services.screenshot_service import ScreenshotService

# Import our specialized modules
from event_manager import EventManager, EventType
from websocket_handler import WebSocketHandler
from capture_manager import CaptureManager
from training_export import TrainingExportCoordinator
from executor_core import ExecutorCore
from gui_automation import GUIAutomation
from services.web_extraction_service import WebExtractionService

# Check if qontinui library is available
try:
    from qontinui import Find, Image, Location, navigation_api, registry
    from qontinui.config import get_settings
    from qontinui.reporting import register_callback, EventType as QontinuiEventType

    QONTINUI_AVAILABLE = True
except ImportError as e:
    QONTINUI_AVAILABLE = False
    import_error_details = f"{type(e).__name__}: {str(e)}"
    full_traceback = traceback.format_exc()
    print(
        json.dumps(
            {
                "type": "event",
                "event": "error",
                "timestamp": time.time(),
                "sequence": 0,
                "data": {
                    "message": "Qontinui library not available. Please install qontinui package.",
                    "details": import_error_details,
                    "qontinui_path": str(qontinui_src_path),
                    "path_exists": qontinui_src_path.exists(),
                    "full_traceback": full_traceback,
                },
            }
        ),
        flush=True,
    )


class StateMemoryAdapter:
    """Adapter to bridge StateExecutor interface to UnifiedDataCollector's expected interface."""

    def __init__(self, state_executor):
        self.state_executor = state_executor

    def get_active_state_names(self) -> list[str]:
        if self.state_executor is None:
            return []
        return self.state_executor.get_active_states()


class QontinuiExecutor:
    """
    Main executor that composes specialized modules.

    Responsibilities:
    - Coordinate all specialized modules
    - Handle commands from Rust bridge
    - Maintain execution state
    - Provide workflow execution interface
    """

    def __init__(self):
        """Initialize executor and all modules."""
        self.config = None
        self.is_running = False
        self._navigation_sequence = 0

        # Initialize EventManager first (other modules depend on it)
        self.event_manager = EventManager()

        # Initialize WebSocketHandler with command handler
        self.websocket_handler = WebSocketHandler(
            emit_log_fn=self.event_manager.emit_log, on_command_fn=self._handle_websocket_command
        )

        # Initialize CaptureManager
        self.capture_manager = CaptureManager(
            emit_log_fn=self.event_manager.emit_log, emit_event_fn=self.event_manager.emit_event
        )

        # Initialize TrainingExportCoordinator
        self.training_export = TrainingExportCoordinator(emit_log_fn=self.event_manager.emit_log)

        # Initialize ExecutorCore
        self.executor_core = ExecutorCore(
            emit_log_fn=self.event_manager.emit_log, emit_event_fn=self.event_manager.emit_event
        )

        # Execution tree for hierarchical tracking
        self.execution_tree = ExecutionTree()

        # Unified data collector (initialized after config load)
        self.unified_data_collector = None
        self.screenshot_service = None

        # GUIAutomation (initialized after config load)
        self.gui_automation = None

        # Web extraction service (lazy-loaded when needed)
        self._web_extraction_service = None

        # Dedicated event loop for async operations (avoids conflicts with WebSocket thread's loop)
        # Runs in a background thread to allow run_coroutine_threadsafe()
        self._async_loop = None
        self._async_thread = None

        # EventTranslator for library callbacks (initialized if library available)
        if QONTINUI_AVAILABLE:
            self.event_translator = EventTranslator(
                self._emit_event_wrapper,
                state_lookup=self._get_state_for_image,
                hierarchy_lookup=self._get_current_hierarchy,
                image_data_lookup=self._get_image_data,
            )
            self.event_translator.register_all_callbacks()

            # Verify callbacks were registered
            from qontinui.reporting import get_event_registry

            event_registry = get_event_registry()
            logger.debug(f"[INIT] Event registry has_listeners: {event_registry.has_listeners}")
            logger.debug(f"[INIT] EventTranslator initialized and callbacks registered")

        logger.info(f"QontinuiExecutor initialized (library_available={QONTINUI_AVAILABLE})")

    def _emit_event_wrapper(self, event_type: str, data: Dict[str, Any]):
        """
        Wrapper for EventTranslator to emit events.

        Also forwards automation events to WebSocket if enabled.
        """
        # Forward to event manager
        self.event_manager.emit_event_wrapper(event_type, data)

        # Forward to WebSocket if enabled
        if self.websocket_handler.is_enabled():
            self._forward_to_websocket(event_type, data)

    def _forward_to_websocket(self, event_type: str, data: Dict[str, Any]):
        """Forward events to WebSocket backend."""
        # Handle image recognition events
        if event_type == "image_recognition":
            screenshot_base64 = data.get("screenshot_base64")
            if screenshot_base64:
                metadata = {
                    "event_type": "image_recognition",
                    "image_id": data.get("image_id"),
                    "state_name": data.get("state_name"),
                    "found": data.get("found"),
                    "confidence": data.get("confidence"),
                    "threshold": data.get("threshold"),
                    "match_location": data.get("match_location"),
                    "action_type": "find_image",
                }
                self.websocket_handler.send_screenshot(
                    image=screenshot_base64,
                    name=f"match_{data.get('image_id', 'unknown')}_{int(time.time())}",
                    metadata=metadata,
                )

            self.websocket_handler.send_log(
                level="info" if data.get("found") else "debug",
                message=f"Image recognition: {data.get('image_id')} - {'found' if data.get('found') else 'not found'}",
                log_data={
                    "event_type": "image_recognition",
                    "image_id": data.get("image_id"),
                    "found": data.get("found"),
                    "confidence": data.get("confidence"),
                },
            )
            return

        # Handle other automation events
        level_map = {
            "action_started": "info",
            "action_completed": "info",
            "action_execution": "info",
            "match_found": "info",
            "screenshot_taken": "debug",
            "log": data.get("level", "info"),
        }
        level = level_map.get(event_type, "info")

        # Create message based on event type
        if event_type == "action_started":
            message = f"Action started: {data.get('action_type', 'unknown')}"
        elif event_type == "action_completed":
            success = data.get("success", True)
            action_type = data.get("action_type", "unknown")
            message = f"Action {'completed' if success else 'failed'}: {action_type}"
            if not success:
                level = "error"
        elif event_type == "action_execution":
            message = f"Executed: {data.get('action_type', 'unknown')}"
        elif event_type == "match_found":
            message = f"Match found: {data.get('image_id', 'unknown')}"
        else:
            message = data.get("message", f"Event: {event_type}")

        self.websocket_handler.send_log(
            level=level, message=message, log_data={"event_type": event_type, **data}
        )

    def _get_current_hierarchy(self) -> Dict[str, Any]:
        """Get current execution hierarchy from execution tree."""
        return self.execution_tree.get_current_hierarchy()

    def _get_state_for_image(self, image_id: str) -> Optional[str]:
        """Find which state an image belongs to."""
        if not self.config:
            return None

        states = self.config.get("states", [])
        for state in states:
            state_name = state.get("name")
            state_images = state.get("stateImages", [])

            for state_image in state_images:
                state_image_id = state_image.get("id")
                if state_image_id == image_id:
                    return state_name

                patterns = state_image.get("patterns", [])
                for pattern in patterns:
                    pattern_image_id = pattern.get("image")
                    if pattern_image_id == image_id:
                        return state_name

        return None

    def _get_image_name(self, image_id: str) -> Optional[str]:
        """Get the human-readable name for an image ID."""
        if not self.config:
            return None

        states = self.config.get("states", [])
        for state in states:
            state_images = state.get("stateImages", [])
            for state_image in state_images:
                if state_image.get("id") == image_id:
                    return state_image.get("name")

                patterns = state_image.get("patterns", [])
                for pattern in patterns:
                    if pattern.get("image") == image_id:
                        return state_image.get("name")

        return None

    def _get_image_data(self, image_id: str) -> Optional[str]:
        """Get base64 image data for an image ID."""
        if not self.config:
            return None

        images = self.config.get("images", [])
        for image in images:
            if image.get("id") == image_id:
                return image.get("data")

        return None

    def _initialize_unified_data_services(self):
        """Initialize unified data architecture services."""
        if not QONTINUI_AVAILABLE:
            return

        try:
            # Create run directory
            temp_dir = self.executor_core.get_temp_dir()
            if temp_dir:
                run_dir = Path(temp_dir) / "run_data"
            else:
                run_dir = Path(tempfile.mkdtemp(prefix="qontinui_run_"))

            run_dir.mkdir(parents=True, exist_ok=True)

            # Initialize ScreenshotService
            self.screenshot_service = ScreenshotService(storage_dir=run_dir, enabled=True)
            self.event_manager.emit_log("info", f"ScreenshotService initialized: {run_dir}")

            # Create state memory adapter
            state_memory_adapter = StateMemoryAdapter(self.executor_core.state_executor)

            # Initialize TrainingExportService
            self.training_export.initialize(run_dir)

            # Initialize UnifiedDataCollector
            record_callback = self.training_export.get_record_callback()

            self.unified_data_collector = UnifiedDataCollector(
                state_memory=state_memory_adapter,
                screenshot_service=self.screenshot_service,
                record_created_callback=record_callback,
            )
            self.event_manager.emit_log("info", "UnifiedDataCollector initialized")

            # Connect UnifiedDataCollector to EventTranslator
            if hasattr(self, "event_translator") and self.event_translator:
                self.event_translator.collector = self.unified_data_collector
                self.event_manager.emit_log(
                    "info", "EventTranslator connected to UnifiedDataCollector"
                )

        except Exception as e:
            self.event_manager.emit_log("error", f"Failed to initialize unified data services: {e}")
            self.event_manager.emit_log("debug", f"Traceback: {traceback.format_exc()}")
            self.screenshot_service = None
            self.unified_data_collector = None

    def load_configuration(self, config_path: str) -> bool:
        """
        Load configuration from file.

        Args:
            config_path: Path to JSON configuration file

        Returns:
            True if successful, False otherwise
        """
        success = self.executor_core.load_configuration(config_path)

        if success:
            # Store reference to config
            self.config = self.executor_core.config

            # Initialize unified data services
            self._initialize_unified_data_services()

            # Initialize GUIAutomation with loaded components
            self.gui_automation = GUIAutomation(
                emit_log_fn=self.event_manager.emit_log,
                emit_tree_event_fn=self.event_manager.emit_tree_event,
                execution_tree=self.execution_tree,
                unified_data_collector=self.unified_data_collector,
                action_executor=self.executor_core.action_executor,
                state_executor=self.executor_core.state_executor,
                workflows=self.executor_core.workflows,
                images=self.executor_core.images,
                get_image_name_fn=self._get_image_name,
                get_action_definition_fn=get_action_definition,
            )

            # Inject self as workflow executor for navigation
            if QONTINUI_AVAILABLE:
                navigation_api.set_workflow_executor(self)
                self.event_manager.emit_log("info", "Runner injected as workflow_executor")

        return success

    def execute_workflow(
        self, workflow_id: str, transition_context: Optional[Dict] = None
    ) -> Dict[str, Any]:
        """
        Execute a workflow.

        This method is called by navigation system for transitions.

        Args:
            workflow_id: ID of workflow to execute
            transition_context: Optional transition metadata

        Returns:
            Dict with 'success' key
        """
        if not self.gui_automation:
            return {"success": False, "error": "GUI automation not initialized"}

        try:
            success = self.gui_automation.execute_workflow(workflow_id, transition_context)
            return {"success": success}
        except Exception as e:
            self.event_manager.emit_log("error", f"Workflow execution failed: {e}")
            return {"success": False, "error": str(e)}

    def start_execution(self, workflow_id: str) -> bool:
        """Start workflow execution in background thread."""
        if self.is_running:
            self.event_manager.emit_log("warning", "Execution already in progress")
            return False

        if not self.gui_automation:
            self.event_manager.emit_log(
                "error", "GUI automation not initialized - load config first"
            )
            return False

        self.is_running = True
        self.gui_automation.set_running(True)

        # Start WebSocket session if enabled
        if self.websocket_handler.is_enabled():
            self.websocket_handler.start_session(config_snapshot=self.config)

        # Start execution in background thread
        thread = threading.Thread(target=self._run_workflow, args=(workflow_id,), daemon=True)
        thread.start()

        self.event_manager.emit_event(
            EventType.EXECUTION_STARTED, {"workflow_id": workflow_id, "timestamp": time.time()}
        )

        return True

    def _run_workflow(self, workflow_id: str):
        """Run workflow in background thread."""
        try:
            self.event_manager.emit_log("info", f"Starting workflow execution: {workflow_id}")

            # Reset navigation state
            if self.gui_automation:
                self.gui_automation.reset_navigation_state()

            # Execute workflow
            success = self.gui_automation.execute_workflow(workflow_id)

            self.event_manager.emit_event(
                EventType.EXECUTION_COMPLETED,
                {
                    "success": success,
                    "workflow_id": workflow_id,
                },
            )

            # End WebSocket session
            if self.websocket_handler.is_enabled():
                self.websocket_handler.end_session(status="completed" if success else "failed")

        except Exception as e:
            self.event_manager.emit_log("error", f"Workflow execution error: {e}")
            self.event_manager.emit_log("debug", f"Traceback: {traceback.format_exc()}")

            self.event_manager.emit_event(
                EventType.EXECUTION_COMPLETED,
                {
                    "success": False,
                    "workflow_id": workflow_id,
                    "error": str(e),
                },
            )

            # End WebSocket session with error
            if self.websocket_handler.is_enabled():
                self.websocket_handler.end_session(status="failed", error=str(e))

        finally:
            self.is_running = False
            self.gui_automation.set_running(False)

    def stop_execution(self):
        """Stop the current execution."""
        if self.is_running:
            self.event_manager.emit_log("info", "Stopping execution...")
            self.is_running = False

            if self.gui_automation:
                self.gui_automation.set_running(False)

            self.event_manager.emit_event(
                EventType.EXECUTION_COMPLETED, {"success": False, "reason": "User stopped"}
            )

            # Export training data if enabled
            if self.training_export.is_enabled():
                self.event_manager.emit_log("info", "Exporting training data on stop...")
                self.training_export.export_data()

    def navigate_to_state(self, target_state_id: str) -> Dict[str, Any]:
        """Navigate to a target state via navigation API."""
        self.event_manager.emit_log(
            "info", f"[NAVIGATE] navigate_to_state called: {target_state_id}"
        )

        if not QONTINUI_AVAILABLE:
            return {"success": False, "error": "Qontinui library not available"}

        try:
            # Create navigation node
            nav_node = ExecutionNode(
                id=f"nav_{self._navigation_sequence}",
                node_type="workflow",
                name=f"Navigate to {target_state_id}",
                timestamp=time.time(),
                metadata={"target_state": target_state_id},
                parent=None,
            )
            self._navigation_sequence += 1

            # Emit workflow_started
            self.event_manager.emit_tree_event("workflow_started", nav_node, None)

            # Navigate
            result = navigation_api.open_state(target_state_id)

            # Update node status
            success = result.get("success", False) if isinstance(result, dict) else result
            nav_node.status = "completed" if success else "failed"
            if not success:
                nav_node.error = (
                    result.get("error", "Navigation failed")
                    if isinstance(result, dict)
                    else "Navigation failed"
                )

            # Emit completion
            self.event_manager.emit_tree_event(
                "workflow_completed" if success else "workflow_failed", nav_node, None
            )

            return {
                "success": success,
                "target_state": target_state_id,
                "active_states": (
                    self.executor_core.state_executor.get_active_states()
                    if self.executor_core.state_executor
                    else []
                ),
                "path": result.get("path", []) if isinstance(result, dict) else [],
            }
        except Exception as e:
            logger.error(f"Failed to navigate to state {target_state_id}: {e}")

            if "nav_node" in locals():
                nav_node.status = "failed"
                nav_node.error = str(e)
                self.event_manager.emit_tree_event("workflow_failed", nav_node, None)

            return {"success": False, "error": str(e)}

    def _handle_websocket_command(self, message: Dict[str, Any]) -> None:
        """
        Handle incoming command from WebSocket backend.

        This is called when the backend sends a command to the runner.
        Commands from the web frontend are forwarded through the backend's
        Redis pub/sub system to the runner's WebSocket connection.

        Args:
            message: Command message with format:
                     {"type": "command", "command": "...", "params": {...}}
        """
        import sys

        print(
            f"[info    ] EXECUTOR: Received WebSocket command: {message}",
            file=sys.stderr,
            flush=True,
        )

        try:
            command_name = message.get("command")
            params = message.get("params", {})

            if not command_name:
                print(
                    f"[error   ] EXECUTOR: No command name in message", file=sys.stderr, flush=True
                )
                return

            # Route the command through our normal command handler
            # by constructing a command dict that handle_command expects
            command_dict = {
                "command": command_name,
                "params": params,
            }

            print(
                f"[info    ] EXECUTOR: Routing to handle_command: {command_name}",
                file=sys.stderr,
                flush=True,
            )
            result = self.handle_command(command_dict)
            print(f"[info    ] EXECUTOR: Command result: {result}", file=sys.stderr, flush=True)

            # Send result back through WebSocket to frontend
            if self.websocket_handler and self.websocket_handler.is_connected:
                import json

                response_message = {
                    "type": "command_response",
                    "data": {
                        "command": command_name,
                        "result": result,
                    },
                }
                self.websocket_handler.send_message(json.dumps(response_message))
                print(
                    f"[info    ] EXECUTOR: Sent command response via WebSocket",
                    file=sys.stderr,
                    flush=True,
                )
            else:
                print(
                    f"[warning ] EXECUTOR: WebSocket not connected, cannot send response",
                    file=sys.stderr,
                    flush=True,
                )

        except Exception as e:
            print(
                f"[error   ] EXECUTOR: Error handling WebSocket command: {e}",
                file=sys.stderr,
                flush=True,
            )
            import traceback

            print(
                f"[error   ] EXECUTOR: Traceback: {traceback.format_exc()}",
                file=sys.stderr,
                flush=True,
            )

            # Send error response back through WebSocket
            if self.websocket_handler and self.websocket_handler.is_connected:
                import json

                error_response = {
                    "type": "command_response",
                    "data": {
                        "command": message.get("command", "unknown"),
                        "result": {"success": False, "error": str(e)},
                    },
                }
                self.websocket_handler.send_message(json.dumps(error_response))
                print(
                    f"[info    ] EXECUTOR: Sent error response via WebSocket",
                    file=sys.stderr,
                    flush=True,
                )

    def handle_command(self, command: Dict[str, Any]) -> Dict[str, Any]:
        """Handle command from Rust bridge."""
        cmd_type = command.get("command")
        params = command.get("params", {})

        if cmd_type != "ping":
            self.event_manager.emit_log("info", f"handle_command: received '{cmd_type}'")

        if cmd_type == "load":
            config_path = params.get("config_path")
            success = self.load_configuration(config_path)
            return {"success": success}

        elif cmd_type == "start":
            workflow_id = params.get("workflow_id")
            success = self.start_execution(workflow_id)
            return {"success": success}

        elif cmd_type == "stop":
            self.stop_execution()
            return {"success": True}

        elif cmd_type == "status":
            return {
                "success": True,
                "is_running": self.is_running,
                "config_loaded": self.config is not None,
                "library_available": QONTINUI_AVAILABLE,
            }

        elif cmd_type == "set_debug_settings":
            settings = params.get("settings", {})
            self.executor_core.apply_debug_settings(settings)
            return {"success": True}

        elif cmd_type == "update_capture_settings":
            settings = params.get("settings", {})
            return self.capture_manager.update_settings(settings)

        elif cmd_type == "manual_capture_status":
            return {"success": True, "is_running": self.capture_manager.is_manual_capture_running()}

        elif cmd_type == "ws_configure":
            # Direct stderr output for debugging (use [info] format so Rust logs at info level)
            import sys

            print(
                f"[info    ] WS_DEBUG: ws_configure received! NEW CODE IS RUNNING!",
                file=sys.stderr,
                flush=True,
            )
            enabled = params.get("enabled", False)
            api_url = params.get("api_url", "")
            token = params.get("jwt_token", "")
            project_id = params.get("project_id")
            runner_name = params.get("runner_name")
            print(
                f"[info    ] WS_DEBUG: enabled={enabled}, api_url={api_url}, project_id={project_id}, runner_name={runner_name}, token_len={len(token) if token else 0}",
                file=sys.stderr,
                flush=True,
            )
            self.event_manager.emit_log(
                "info",
                f"[WS_CONFIGURE] enabled={enabled}, api_url={api_url}, project_id={project_id}, runner_name={runner_name}, token_len={len(token) if token else 0}",
            )
            success = self.websocket_handler.configure(
                enabled, api_url, token, project_id, runner_name
            )
            print(
                f"[info    ] WS_DEBUG: configure result: success={success}",
                file=sys.stderr,
                flush=True,
            )
            self.event_manager.emit_log("info", f"[WS_CONFIGURE] Result: success={success}")
            return {"success": success}

        elif cmd_type == "ws_connect":
            import sys

            print(f"[info    ] WS_DEBUG: ws_connect received!", file=sys.stderr, flush=True)
            success = self.websocket_handler.connect()
            print(
                f"[info    ] WS_DEBUG: ws_connect result: success={success}",
                file=sys.stderr,
                flush=True,
            )
            return {"success": success}

        elif cmd_type == "ws_disconnect":
            self.websocket_handler.disconnect()
            return {"success": True}

        elif cmd_type == "ws_start_session":
            config_snapshot = params.get("config_snapshot")
            success = self.websocket_handler.start_session(config_snapshot)
            return {"success": success}

        elif cmd_type == "ws_end_session":
            status = params.get("status", "completed")
            error = params.get("error")
            success = self.websocket_handler.end_session(status, error)
            return {"success": success}

        elif cmd_type == "ws_status":
            return {"success": True, "enabled": self.websocket_handler.is_enabled()}

        elif cmd_type == "ping":
            pong_message = {"type": "pong", "timestamp": time.time()}
            print(json.dumps(pong_message), flush=True)
            return {"success": True}

        elif cmd_type == "navigate_to_state":
            return self.navigate_to_state(params.get("state_id"))

        # Web extraction commands
        elif cmd_type == "start_web_extraction":
            return self._handle_start_web_extraction(params)

        elif cmd_type == "stop_web_extraction":
            return self._handle_stop_web_extraction()

        elif cmd_type == "get_extraction_status":
            return self._handle_get_extraction_status()

        else:
            return {"success": False, "error": f"Unknown command: {cmd_type}"}

    def _get_web_extraction_service(self) -> WebExtractionService:
        """Get or create the web extraction service."""
        if self._web_extraction_service is None:
            self._web_extraction_service = WebExtractionService(
                event_manager=self.event_manager,
                websocket_handler=self.websocket_handler,
            )
        return self._web_extraction_service

    def _start_async_loop(self):
        """Start the async event loop in a background thread."""
        import asyncio

        self._async_loop = asyncio.new_event_loop()
        asyncio.set_event_loop(self._async_loop)
        self._async_loop.run_forever()

    def _get_or_create_async_loop(self):
        """
        Get or create a dedicated event loop for async operations.

        This runs an event loop in a background thread, allowing us to use
        asyncio.run_coroutine_threadsafe() to schedule coroutines from any thread
        (including from within the WebSocket handler's event loop).

        Returns:
            asyncio.AbstractEventLoop: A dedicated event loop for this executor.
        """
        import asyncio
        import threading
        import time

        # Start background thread if needed
        if self._async_thread is None or not self._async_thread.is_alive():
            self._async_loop = None  # Reset loop since thread died
            self._async_thread = threading.Thread(target=self._start_async_loop, daemon=True)
            self._async_thread.start()

            # Wait for loop to be ready
            timeout = 5
            start_time = time.time()
            while self._async_loop is None and time.time() - start_time < timeout:
                time.sleep(0.05)

            if self._async_loop is None:
                raise RuntimeError("Failed to start async event loop")

        return self._async_loop

    def _handle_start_web_extraction(self, params: Dict[str, Any]) -> Dict[str, Any]:
        """Handle start web extraction command."""
        import asyncio
        import sys

        print(
            f"[info    ] EXECUTOR: _handle_start_web_extraction called with params: {params}",
            file=sys.stderr,
            flush=True,
        )

        try:
            service = self._get_web_extraction_service()

            # Get dedicated event loop running in background thread
            loop = self._get_or_create_async_loop()

            # Schedule the async operation on the background loop using run_coroutine_threadsafe
            # This is required because we may be called from within an already-running event loop
            config = params.get("config", params)  # Support both nested and flat config
            print(
                f"[info    ] EXECUTOR: Starting extraction with config: {config}",
                file=sys.stderr,
                flush=True,
            )

            future = asyncio.run_coroutine_threadsafe(service.start_extraction(config), loop)
            # Wait for result with timeout (extraction may take a while to initialize)
            result = future.result(timeout=60)

            if result.get("success"):
                self.event_manager.emit_event(
                    EventType.EXTRACTION_STARTED,
                    {
                        "extraction_id": result.get("extraction_id"),
                        "config": config,
                    },
                )

            print(
                f"[info    ] EXECUTOR: start_extraction result: {result}",
                file=sys.stderr,
                flush=True,
            )
            return result

        except Exception as e:
            print(
                f"[error   ] EXECUTOR: Failed to start extraction: {e}", file=sys.stderr, flush=True
            )
            import traceback

            print(
                f"[error   ] EXECUTOR: Traceback: {traceback.format_exc()}",
                file=sys.stderr,
                flush=True,
            )
            self.event_manager.emit_log("error", f"Failed to start extraction: {e}")
            return {"success": False, "error": str(e)}

    def _handle_stop_web_extraction(self) -> Dict[str, Any]:
        """Handle stop web extraction command."""
        import asyncio

        try:
            if self._web_extraction_service is None:
                return {"success": False, "error": "No extraction in progress"}

            # Get dedicated event loop running in background thread
            loop = self._get_or_create_async_loop()

            # Schedule the async operation on the background loop using run_coroutine_threadsafe
            future = asyncio.run_coroutine_threadsafe(
                self._web_extraction_service.stop_extraction(), loop
            )
            result = future.result(timeout=30)
            return result

        except Exception as e:
            self.event_manager.emit_log("error", f"Failed to stop extraction: {e}")
            return {"success": False, "error": str(e)}

    def _handle_get_extraction_status(self) -> Dict[str, Any]:
        """Handle get extraction status command."""
        try:
            if self._web_extraction_service is None:
                return {
                    "success": True,
                    "is_running": False,
                    "extraction_id": None,
                }

            status = self._web_extraction_service.get_status()
            return {"success": True, **status}

        except Exception as e:
            self.event_manager.emit_log("error", f"Failed to get extraction status: {e}")
            return {"success": False, "error": str(e)}

    def __del__(self):
        """Clean up resources on exit."""
        # Stop capture manager
        if hasattr(self, "capture_manager") and self.capture_manager:
            if self.capture_manager.manual_click_listener:
                try:
                    self.capture_manager.manual_click_listener.cleanup()
                except Exception:
                    pass

        # Clean up executor core
        if hasattr(self, "executor_core") and self.executor_core:
            self.executor_core.cleanup()


def main():
    """Main entry point for the Qontinui executor."""
    executor = QontinuiExecutor()

    executor.event_manager.emit_log(
        "info", "Python executor main loop started, waiting for commands"
    )

    # Read commands from stdin
    for line in sys.stdin:
        try:
            command = json.loads(line.strip())
            cmd_name = command.get("command", "unknown")

            if cmd_name != "ping":
                executor.event_manager.emit_log("info", f"Received command: {cmd_name}")

            if command.get("type") == "command":
                response = executor.handle_command(command)
                response["id"] = command.get("id")
                response["type"] = "response"

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
