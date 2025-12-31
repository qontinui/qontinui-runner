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

import contextlib
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

from action_definitions import get_action_definition  # noqa: E402
from capture_manager import CaptureManager  # noqa: E402

# Import our specialized modules
from event_manager import EventManager, EventType  # noqa: E402

# CRITICAL: Import local python-bridge modules BEFORE qontinui library
from event_translator import EventTranslator  # noqa: E402
from execution_tree import ExecutionNode, ExecutionTree  # noqa: E402
from executor_core import ExecutorCore  # noqa: E402
from gui_automation import GUIAutomation  # noqa: E402
from services.input_monitor_service import InputMonitorService  # noqa: E402
from services.screenshot_service import ScreenshotService  # noqa: E402
from services.unified_data_collector import UnifiedDataCollector  # noqa: E402
from services.web_extraction_service import WebExtractionService  # noqa: E402
from test_results_handler import TestResultsHandler  # noqa: E402
from training_export import TrainingExportCoordinator  # noqa: E402
from websocket_handler import WebSocketHandler  # noqa: E402

# Check if qontinui library is available
try:
    from qontinui import navigation_api  # noqa: E402, F401
    from qontinui.config import get_settings  # noqa: E402

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
        return self.state_executor.get_active_states()  # type: ignore[no-any-return]


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
        self.target_monitor: int | None = None  # Monitor index for execution

        # Initialize EventManager first (other modules depend on it)
        self.event_manager = EventManager()

        # Initialize WebSocketHandler with command handler
        self.websocket_handler = WebSocketHandler(
            emit_log_fn=self.event_manager.emit_log, on_command_fn=self._handle_websocket_command
        )

        # Initialize TestResultsHandler for QA dashboard submission
        self.test_results_handler = TestResultsHandler(emit_log_fn=self.event_manager.emit_log)

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

        # InputMonitorService for validation capture (initialized on demand)
        self.input_monitor_service = None
        # Flag to enable input capture during workflow execution
        self.capture_input_for_validation = False
        self._input_capture_session_id: str | None = None

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
            logger.debug("[INIT] EventTranslator initialized and callbacks registered")

        logger.info(f"QontinuiExecutor initialized (library_available={QONTINUI_AVAILABLE})")

    def _emit_event_wrapper(self, event_type: str, data: dict[str, Any]):
        """
        Wrapper for EventTranslator to emit events.

        Also forwards automation events to WebSocket if enabled.
        """
        # Forward to event manager
        self.event_manager.emit_event_wrapper(event_type, data)

        # Forward to WebSocket if enabled
        if self.websocket_handler.is_enabled():
            self._forward_to_websocket(event_type, data)

    def _forward_to_websocket(self, event_type: str, data: dict[str, Any]):
        """Forward events to WebSocket backend."""
        import sys

        print(f"[FWD_TO_WS] Called: event_type={event_type}", file=sys.stderr, flush=True)
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

    def _get_current_hierarchy(self) -> dict[str, Any]:
        """Get current execution hierarchy from execution tree."""
        return self.execution_tree.get_current_hierarchy()  # type: ignore[no-any-return]

    def _get_state_for_image(self, image_id: str) -> str | None:
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
                    return state_name  # type: ignore[no-any-return]

                patterns = state_image.get("patterns", [])
                for pattern in patterns:
                    pattern_image_id = pattern.get("image")
                    if pattern_image_id == image_id:
                        return state_name  # type: ignore[no-any-return]

        return None

    def _get_image_name(self, image_id: str) -> str | None:
        """Get the human-readable name for an image ID."""
        if not self.config:
            return None

        states = self.config.get("states", [])
        for state in states:
            state_images = state.get("stateImages", [])
            for state_image in state_images:
                if state_image.get("id") == image_id:
                    return state_image.get("name")  # type: ignore[no-any-return]

                patterns = state_image.get("patterns", [])
                for pattern in patterns:
                    if pattern.get("image") == image_id:
                        return state_image.get("name")  # type: ignore[no-any-return]

        return None

    def _get_image_data(self, image_id: str) -> str | None:
        """Get base64 image data for an image ID."""
        if not self.config:
            return None

        images = self.config.get("images", [])
        for image in images:
            if image.get("id") == image_id:
                return image.get("data")  # type: ignore[no-any-return]

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

            # Initialize UnifiedDataCollector with combined callback
            training_callback = self.training_export.get_record_callback()

            def combined_record_callback(record):
                """Callback that reports to training export and test results handler."""
                # Call training export callback
                if training_callback:
                    training_callback(record)

                # Report action data for historical indexing (Config Testing)
                if self.test_results_handler and self.test_results_handler.is_enabled():
                    try:
                        # Extract match info from record
                        match_summary = record.match_summary or {}

                        self.test_results_handler.report_action(
                            action_id=record.action_id,
                            action_type=record.action_type,
                            success=record.success,
                            pattern_id=match_summary.get("image_id"),
                            pattern_name=match_summary.get("image_id"),  # Using image_id as name
                            active_states=list(record.active_states_before),
                            match_count=1 if match_summary.get("found") else 0,
                            best_match_score=match_summary.get("confidence"),
                            match_x=(
                                match_summary.get("location", {}).get("x")
                                if match_summary.get("location")
                                else None
                            ),
                            match_y=(
                                match_summary.get("location", {}).get("y")
                                if match_summary.get("location")
                                else None
                            ),
                            match_width=None,  # Not available in current record
                            match_height=None,
                            duration_ms=int(record.duration_ms) if record.duration_ms else None,
                            result_data={
                                "config": record.config,
                                "clicked_location": record.clicked_location,
                                "transition_data": record.transition_data,
                            },
                        )
                    except Exception as e:
                        self.event_manager.emit_log(
                            "debug", f"Failed to report action for historical indexing: {e}"
                        )

            self.unified_data_collector = UnifiedDataCollector(
                state_memory=state_memory_adapter,
                screenshot_service=self.screenshot_service,
                record_created_callback=combined_record_callback,
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
        # DEBUG: Log config loading
        import os
        import tempfile

        debug_log_path = os.path.join(tempfile.gettempdir(), "qontinui_load_config_debug.log")
        try:
            with open(debug_log_path, "a") as f:
                from datetime import datetime

                f.write(f"\n=== LOAD_CONFIGURATION DEBUG {datetime.now()} ===\n")
                f.write(f"config_path: {config_path}\n")
                f.write(f"QONTINUI_AVAILABLE: {QONTINUI_AVAILABLE}\n")
                f.flush()
        except Exception:
            pass

        success = self.executor_core.load_configuration(config_path)

        # DEBUG: Log result
        try:
            with open(debug_log_path, "a") as f:
                f.write(f"executor_core.load_configuration returned: {success}\n")
                f.write(f"executor_core.action_executor: {self.executor_core.action_executor}\n")
                f.write(f"executor_core.state_executor: {self.executor_core.state_executor}\n")
                f.flush()
        except Exception:
            pass

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

        return success  # type: ignore[no-any-return]

    def _start_input_capture_for_execution(self, session_id: str) -> bool:
        """Start input capture for coordinate validation during execution.

        Args:
            session_id: Session ID for this capture session

        Returns:
            True if started successfully
        """
        if not self.capture_input_for_validation:
            return False

        try:
            if self.input_monitor_service is None:
                dev_logs_dir = Path(__file__).parent.parent.parent / ".dev-logs"
                dev_logs_dir.mkdir(parents=True, exist_ok=True)
                self.input_monitor_service = InputMonitorService(storage_dir=dev_logs_dir)
                self.event_manager.emit_log(
                    "info", f"InputMonitorService initialized: {dev_logs_dir}"
                )

            self.input_monitor_service.start_monitoring(session_id=session_id, fps=30)
            self._input_capture_session_id = session_id
            self.event_manager.emit_log(
                "info", f"Input capture started for execution validation: session={session_id}"
            )
            return True
        except Exception as e:
            self.event_manager.emit_log("error", f"Failed to start input capture: {e}")
            return False

    def _stop_input_capture_for_execution(self) -> dict[str, Any] | None:
        """Stop input capture and return results.

        Returns:
            Dict with events_file and events_count, or None if not running
        """
        if self.input_monitor_service is None or not self._input_capture_session_id:
            return None

        try:
            events_file = self.input_monitor_service.stop_monitoring()
            events_count = len(self.input_monitor_service.get_events())
            session_id = self._input_capture_session_id
            self._input_capture_session_id = None

            self.event_manager.emit_log(
                "info",
                f"Input capture stopped: {events_count} events captured, file={events_file}",
            )
            return {
                "session_id": session_id,
                "events_file": str(events_file) if events_file else None,
                "events_count": events_count,
            }
        except Exception as e:
            self.event_manager.emit_log("error", f"Failed to stop input capture: {e}")
            self._input_capture_session_id = None
            return None

    def execute_workflow(
        self, workflow_id: str, transition_context: dict | None = None
    ) -> dict[str, Any]:
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

    def start_execution(
        self,
        workflow_id: str,
        monitor: int | None = None,
        monitor_offset_x: int | None = None,
        monitor_offset_y: int | None = None,
        initial_state_ids: list[str] | None = None,
    ) -> bool:
        """Start workflow execution in background thread.

        Args:
            workflow_id: ID of the workflow to execute
            monitor: Monitor index to use for screen capture and actions (None = default)
            monitor_offset_x: DEPRECATED - X offset (ignored, library looks up internally)
            monitor_offset_y: DEPRECATED - Y offset (ignored, library looks up internally)
            initial_state_ids: Resolved initial active states from runner (overrides workflow config)
        """
        # Store initial_state_ids for use in _run_workflow and event emission
        self._initial_state_ids = initial_state_ids
        self.event_manager.emit_log(
            "info",
            f"[PYTHON_EXECUTOR] start_execution called: workflow_id={workflow_id}, monitor={monitor}",
        )
        # Write to debug file for monitor tracing
        try:
            with open(
                os.path.join(tempfile.gettempdir(), "qontinui_monitor_debug.log"),
                "a",
                encoding="utf-8",
            ) as f:
                timestamp = datetime.now().strftime("%Y-%m-%d %H:%M:%S.%f")[:-3]
                f.write(
                    f"[{timestamp}] [PYTHON_EXECUTOR] start_execution called: workflow_id={workflow_id}, monitor={monitor}\n"
                )
        except Exception:
            pass

        if self.is_running:
            self.event_manager.emit_log("warning", "Execution already in progress")
            return False

        if not self.gui_automation:
            self.event_manager.emit_log(
                "error", "GUI automation not initialized - load config first"
            )
            return False

        # Store monitor selection for use in actions
        self.target_monitor = monitor
        self.event_manager.emit_log(
            "info", f"[PYTHON_EXECUTOR] Set self.target_monitor = {monitor}"
        )
        # Write to debug file for monitor tracing
        try:
            with open(
                os.path.join(tempfile.gettempdir(), "qontinui_monitor_debug.log"),
                "a",
                encoding="utf-8",
            ) as f:
                timestamp = datetime.now().strftime("%Y-%m-%d %H:%M:%S.%f")[:-3]
                f.write(f"[{timestamp}] [PYTHON_EXECUTOR] Set self.target_monitor = {monitor}\n")
        except Exception:
            pass
        if monitor is not None:
            self.event_manager.emit_log("info", f"[PYTHON_EXECUTOR] Using monitor index: {monitor}")
            # Apply monitor setting to FrameworkSettings so qontinui core uses this monitor
            if QONTINUI_AVAILABLE:
                try:
                    settings = get_settings()
                    settings.monitor.default_screen_index = monitor
                    self.event_manager.emit_log(
                        "debug", f"Set FrameworkSettings.monitor.default_screen_index = {monitor}"
                    )

                    # Set target monitor on state_executor for coordinate conversion
                    # The library looks up monitor position internally using MSS
                    if self.executor_core and self.executor_core.state_executor:
                        self.executor_core.state_executor.set_monitor(monitor)
                        self.event_manager.emit_log(
                            "debug",
                            f"Set target monitor: {monitor} (library will look up position via MSS)",
                        )
                        # Write to debug file
                        try:
                            with open(
                                os.path.join(tempfile.gettempdir(), "qontinui_monitor_debug.log"),
                                "a",
                                encoding="utf-8",
                            ) as f:
                                timestamp = datetime.now().strftime("%Y-%m-%d %H:%M:%S.%f")[:-3]
                                f.write(
                                    f"[{timestamp}] [PYTHON_EXECUTOR] Set target monitor: {monitor}\n"
                                )
                        except Exception:
                            pass
                    else:
                        # Debug: Log why we couldn't set monitor
                        try:
                            with open(
                                os.path.join(tempfile.gettempdir(), "qontinui_monitor_debug.log"),
                                "a",
                                encoding="utf-8",
                            ) as f:
                                timestamp = datetime.now().strftime("%Y-%m-%d %H:%M:%S.%f")[:-3]
                                f.write(
                                    f"[{timestamp}] [PYTHON_EXECUTOR] WARNING: Cannot set monitor - executor_core or state_executor is None\n"
                                )
                        except Exception:
                            pass
                except Exception as e:
                    self.event_manager.emit_log(
                        "warning", f"Failed to set monitor in FrameworkSettings: {e}"
                    )

        self.is_running = True
        self.gui_automation.set_running(True)

        # Start input capture for coordinate validation if enabled
        if self.capture_input_for_validation:
            capture_session_id = f"exec-{workflow_id}-{int(time.time())}"
            self._start_input_capture_for_execution(capture_session_id)

        # Start WebSocket session if enabled or if it was configured (for auto-reconnect)
        # Call start_session even if currently disconnected - it will attempt to reconnect
        ws_is_enabled = self.websocket_handler.is_enabled()
        ws_config_exists = self.websocket_handler.ws_config is not None
        ws_config_enabled = self.websocket_handler.ws_config.enabled if ws_config_exists else False
        self.event_manager.emit_log(
            "debug",
            f"[WS_SESSION_CHECK] is_enabled={ws_is_enabled}, ws_config_exists={ws_config_exists}, ws_config.enabled={ws_config_enabled}, ws_enabled={self.websocket_handler.ws_enabled}",
        )
        if ws_is_enabled or (ws_config_exists and ws_config_enabled):
            self.event_manager.emit_log("info", "[WS_SESSION_CHECK] Starting WebSocket session...")
            result = self.websocket_handler.start_session(config_snapshot=self.config)
            self.event_manager.emit_log(
                "info", f"[WS_SESSION_CHECK] start_session result: {result}"
            )
        else:
            self.event_manager.emit_log(
                "warning", "[WS_SESSION_CHECK] WebSocket session NOT started - conditions not met"
            )

        # Start execution in background thread
        thread = threading.Thread(target=self._run_workflow, args=(workflow_id,), daemon=True)
        thread.start()

        self.event_manager.emit_event(
            EventType.EXECUTION_STARTED,
            {
                "workflow_id": workflow_id,
                "timestamp": time.time(),
                "initial_state_ids": self._initial_state_ids or [],
            },
        )

        return True

    def _run_workflow(self, workflow_id: str):
        """Run workflow in background thread."""
        execution_start_time = time.time()
        test_run_id = None

        try:
            self.event_manager.emit_log("info", f"Starting workflow execution: {workflow_id}")

            # Reset navigation state
            if self.gui_automation:
                self.gui_automation.reset_navigation_state()

            # Get workflow config for test results
            workflow = self.executor_core.workflows.get(workflow_id) if self.executor_core else None
            workflow_name = workflow_id
            if workflow:
                if isinstance(workflow, dict):
                    workflow_name = workflow.get("name", workflow_id)
                elif hasattr(workflow, "name"):
                    workflow_name = workflow.name

            # Start test run for QA dashboard
            if self.test_results_handler.is_enabled():
                test_run_id = self.test_results_handler.start_test_run(
                    workflow_name=workflow_name,
                    workflow_config=self.config or {},
                )

            # Initialize state executor with initial states
            # Priority: resolved from runner (self._initial_state_ids) > workflow config
            if self.executor_core and self.executor_core.state_executor:
                # Use runner-resolved initial states if available
                initial_state_ids = self._initial_state_ids
                if not initial_state_ids and workflow:
                    # Fall back to extracting from workflow config
                    if isinstance(workflow, dict):
                        initial_state_ids = workflow.get("initialStateIds")
                    elif hasattr(workflow, "initial_state_ids"):
                        initial_state_ids = workflow.initial_state_ids

                if initial_state_ids:
                    self.event_manager.emit_log(
                        "info",
                        f"Initializing with initial states: {initial_state_ids}",
                    )
                    self.executor_core.state_executor.initialize(initial_state_ids)
                else:
                    self.executor_core.state_executor.initialize()

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

            # Complete test run for QA dashboard
            if test_run_id and self.test_results_handler.is_enabled():
                execution_duration = time.time() - execution_start_time
                self.test_results_handler.complete_test_run(
                    success=success,
                    summary=f"Workflow '{workflow_name}' {'completed successfully' if success else 'failed'} in {execution_duration:.1f}s",
                )

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

            # Complete test run with failure
            if test_run_id and self.test_results_handler.is_enabled():
                self.test_results_handler.complete_test_run(
                    success=False,
                    summary=f"Workflow failed with error: {e}",
                )

        finally:
            # Stop input capture for coordinate validation
            self._stop_input_capture_for_execution()

            self.is_running = False
            self.gui_automation.set_running(False)

    def stop_execution(self):
        """Stop the current execution."""
        if self.is_running:
            self.event_manager.emit_log("info", "Stopping execution...")
            self.is_running = False

            # Stop input capture for coordinate validation
            self._stop_input_capture_for_execution()

            if self.gui_automation:
                self.gui_automation.set_running(False)

            self.event_manager.emit_event(
                EventType.EXECUTION_COMPLETED, {"success": False, "reason": "User stopped"}
            )

            # Export training data if enabled
            if self.training_export.is_enabled():
                self.event_manager.emit_log("info", "Exporting training data on stop...")
                self.training_export.export_data()

    def navigate_to_state(self, target_state_id: str) -> dict[str, Any]:
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

    def _handle_websocket_command(self, message: dict[str, Any]) -> None:
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
                    "[error   ] EXECUTOR: No command name in message", file=sys.stderr, flush=True
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
                    "[info    ] EXECUTOR: Sent command response via WebSocket",
                    file=sys.stderr,
                    flush=True,
                )
            else:
                print(
                    "[warning ] EXECUTOR: WebSocket not connected, cannot send response",
                    file=sys.stderr,
                    flush=True,
                )

        except Exception as e:
            print(
                f"[error   ] EXECUTOR: Error handling WebSocket command: {e}",
                file=sys.stderr,
                flush=True,
            )

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
                    "[info    ] EXECUTOR: Sent error response via WebSocket",
                    file=sys.stderr,
                    flush=True,
                )

    def handle_command(self, command: dict[str, Any]) -> dict[str, Any]:
        """Handle command from Rust bridge."""
        cmd_type = command.get("command")
        params = command.get("params", {})

        # Don't log high-frequency commands to avoid flooding the logs
        if cmd_type not in ("ping", "status"):
            self.event_manager.emit_log("info", f"handle_command: received '{cmd_type}'")

        if cmd_type == "load":
            config_path = params.get("config_path")
            success = self.load_configuration(config_path)
            return {"success": success}

        elif cmd_type == "start":
            workflow_id = params.get("workflow_id") or params.get("workflow")
            # Support both "monitor" and "monitor_index" parameter names
            # Use explicit None check to handle monitor_index=0 correctly (0 is falsy in Python)
            monitor = params.get("monitor_index")
            if monitor is None:
                monitor = params.get("monitor")  # Monitor index to use
            # Get monitor offset from Rust (if provided)
            monitor_offset_x = params.get("monitor_offset_x")
            monitor_offset_y = params.get("monitor_offset_y")
            # Get resolved initial_state_ids from Rust (if provided)
            initial_state_ids = params.get("initial_state_ids")
            self.event_manager.emit_log(
                "info",
                f"[PYTHON_EXECUTOR] start command: workflow_id={workflow_id}, params={params}, resolved monitor={monitor}",
            )
            # Write to debug file for monitor tracing
            try:
                with open(
                    os.path.join(tempfile.gettempdir(), "qontinui_monitor_debug.log"),
                    "a",
                    encoding="utf-8",
                ) as f:
                    timestamp = datetime.now().strftime("%Y-%m-%d %H:%M:%S.%f")[:-3]
                    f.write(
                        f"[{timestamp}] [PYTHON_EXECUTOR] start command: workflow_id={workflow_id}, params={params}, resolved monitor={monitor}, offset=({monitor_offset_x}, {monitor_offset_y})\n"
                    )
            except Exception:
                pass
            success = self.start_execution(
                workflow_id,
                monitor=monitor,
                monitor_offset_x=monitor_offset_x,
                monitor_offset_y=monitor_offset_y,
                initial_state_ids=initial_state_ids,
            )
            return {"success": success}

        elif cmd_type == "stop":
            self.stop_execution()
            return {"success": True}

        elif cmd_type == "execute_action":
            # Execute a single GUI action (e.g., click on an image)
            action_type = params.get("action_type", "CLICK")
            image_id = params.get("image_id")
            monitor_index = params.get("monitor_index", 0)

            if not image_id:
                return {"success": False, "error": "image_id is required"}

            if not self.gui_automation:
                return {"success": False, "error": "GUI automation not initialized"}

            self.event_manager.emit_log(
                "info",
                f"[EXECUTE_ACTION] Executing {action_type} on image: {image_id}",
            )

            # Build action data for gui_automation.execute_action()
            action_data = {
                "id": f"action-{action_type.lower()}-{time.time()}",
                "type": action_type.upper(),
                "config": {
                    "target": {
                        "type": "image",
                        "imageIds": [image_id],
                    }
                },
            }

            try:
                # Set monitor if provided
                if self.executor_core.state_executor and monitor_index is not None:
                    self.executor_core.state_executor.set_monitor(monitor_index)

                # Execute the action
                success = self.gui_automation.execute_action(action_data)

                self.event_manager.emit_log(
                    "info" if success else "warning",
                    f"[EXECUTE_ACTION] {action_type} on {image_id}: {'success' if success else 'failed'}",
                )

                return {
                    "success": success,
                    "action_type": action_type,
                    "image_id": image_id,
                }
            except Exception as e:
                error_msg = str(e)
                self.event_manager.emit_log(
                    "error",
                    f"[EXECUTE_ACTION] Error executing {action_type} on {image_id}: {error_msg}",
                )
                return {
                    "success": False,
                    "action_type": action_type,
                    "image_id": image_id,
                    "error": error_msg,
                }

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
            return self.capture_manager.update_settings(settings)  # type: ignore[no-any-return]

        elif cmd_type == "manual_capture_status":
            return {"success": True, "is_running": self.capture_manager.is_manual_capture_running()}

        elif cmd_type == "set_input_capture_enabled":
            # Enable/disable input capture for coordinate validation during execution
            # When enabled, input will be automatically captured during workflow execution
            enabled = params.get("enabled", False)
            self.capture_input_for_validation = enabled
            self.event_manager.emit_log(
                "info",
                f"Input capture for validation {'enabled' if enabled else 'disabled'}",
            )
            return {"success": True, "enabled": enabled}

        elif cmd_type == "get_input_validation_status":
            # Get current input validation status
            is_monitoring = (
                self.input_monitor_service is not None
                and self._input_capture_session_id is not None
            )
            events_count = 0
            if self.input_monitor_service and is_monitoring:
                events_count = len(self.input_monitor_service.get_events())
            return {
                "success": True,
                "enabled": self.capture_input_for_validation,
                "is_monitoring": is_monitoring,
                "events_count": events_count,
                "session_id": self._input_capture_session_id,
            }

        elif cmd_type == "ws_configure":
            # Direct stderr output for debugging (use [info] format so Rust logs at info level)
            import sys

            print(
                "[info    ] WS_DEBUG: ws_configure received! NEW CODE IS RUNNING!",
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

            print("[info    ] WS_DEBUG: ws_connect received!", file=sys.stderr, flush=True)
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
            return {
                "success": True,
                "enabled": self.websocket_handler.is_enabled(),
                "connected": self.websocket_handler.is_connected,
            }

        # Test Results Handler commands (for QA Dashboard)
        elif cmd_type == "test_results_configure":
            enabled = params.get("enabled", False)
            api_url = params.get("api_url", "")
            access_token = params.get("access_token", "")
            project_id = params.get("project_id")
            self.event_manager.emit_log(
                "info",
                f"[TEST_RESULTS_CONFIGURE] enabled={enabled}, api_url={api_url}, project_id={project_id}",
            )
            success = self.test_results_handler.configure(
                enabled, api_url, access_token, project_id
            )
            return {"success": success}

        elif cmd_type == "test_results_status":
            return {
                "success": True,
                **self.test_results_handler.get_status(),
            }

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

        # Remote workflow execution from web app
        elif cmd_type == "execute_workflow":
            return self._handle_execute_workflow(params)

        # Screenshot capture command (for direct capture via Python)
        elif cmd_type == "capture_screenshot":
            return self._handle_capture_screenshot(params)

        # SAM3 segmentation command
        elif cmd_type == "segment_screenshot":
            return self._handle_segment_screenshot(params)

        else:
            return {"success": False, "error": f"Unknown command: {cmd_type}"}

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
            self.event_manager.emit_event(
                "screenshot_captured",
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
                params.get("model")

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

    def _get_web_extraction_service(self) -> WebExtractionService:
        """Get or create the web extraction service."""
        if self._web_extraction_service is None:
            self._web_extraction_service = WebExtractionService(
                event_manager=self.event_manager,
                websocket_handler=self.websocket_handler,
            )
        return self._web_extraction_service  # type: ignore[no-any-return]

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

    def _handle_start_web_extraction(self, params: dict[str, Any]) -> dict[str, Any]:
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

            print(
                f"[error   ] EXECUTOR: Traceback: {traceback.format_exc()}",
                file=sys.stderr,
                flush=True,
            )
            self.event_manager.emit_log("error", f"Failed to start extraction: {e}")
            return {"success": False, "error": str(e)}

    def _handle_stop_web_extraction(self) -> dict[str, Any]:
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
            return result  # type: ignore[no-any-return]

        except Exception as e:
            self.event_manager.emit_log("error", f"Failed to stop extraction: {e}")
            return {"success": False, "error": str(e)}

    def _handle_get_extraction_status(self) -> dict[str, Any]:
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

    def _handle_execute_workflow(self, params: dict[str, Any]) -> dict[str, Any]:
        """
        Handle execute_workflow command from web app.

        This command receives a full workflow configuration from the web app,
        writes it to a temporary file, loads it, and starts execution.

        Args:
            params: Dictionary containing:
                - execution_id: Unique ID for tracking this execution
                - workflow: Full workflow configuration (as exported from web app)
                - variables: Optional variables to pass to workflow

        Returns:
            Dictionary with success status and execution details
        """
        import os
        import sys
        import tempfile

        execution_id = params.get("execution_id")
        workflow = params.get("workflow")
        variables = params.get("variables", {})

        print(
            f"[info    ] EXECUTOR: _handle_execute_workflow called with execution_id={execution_id}",
            file=sys.stderr,
            flush=True,
        )

        if not workflow:
            return {"success": False, "error": "No workflow configuration provided"}

        if not execution_id:
            return {"success": False, "error": "No execution_id provided"}

        try:
            # Stop any existing execution
            if self.is_running:
                self.event_manager.emit_log(
                    "info", "Stopping current execution before starting new workflow"
                )
                self.stop_execution()

            # Write workflow to temporary file
            # The workflow from the web app is the full export format, which includes
            # the configuration structure that load_configuration expects
            temp_dir = tempfile.gettempdir()
            temp_file = os.path.join(temp_dir, f"qontinui_remote_{execution_id}.json")

            # If workflow is the export format, it may be the full config
            # If it's just the workflow, wrap it in the expected format
            if "workflows" not in workflow and "stateMachine" not in workflow:
                # Workflow is probably just a single workflow, wrap it
                config_data = {
                    "version": workflow.get("version", "1.0.0"),
                    "workflows": [workflow] if isinstance(workflow, dict) else workflow,
                    "images": [],
                    "settings": {},
                }
            else:
                # It's already in the full config format
                config_data = workflow

            with open(temp_file, "w", encoding="utf-8") as f:
                json.dump(config_data, f, indent=2)

            self.event_manager.emit_log("info", f"Workflow configuration written to {temp_file}")

            # Load the configuration
            load_success = self.load_configuration(temp_file)
            if not load_success:
                # Clean up temp file
                with contextlib.suppress(Exception):
                    os.unlink(temp_file)
                return {"success": False, "error": "Failed to load workflow configuration"}

            # Apply any variables if provided
            if variables:
                self.event_manager.emit_log("info", f"Applying variables: {list(variables.keys())}")

                # Apply variables to the action executor's variable context
                if self.executor_core and self.executor_core.action_executor:
                    try:
                        if hasattr(self.executor_core.action_executor, "variable_context"):
                            for key, value in variables.items():
                                # Set variables with 'global' scope so they're available throughout execution
                                self.executor_core.action_executor.variable_context.set(
                                    key, value, "global"
                                )
                                self.event_manager.emit_log(
                                    "debug",
                                    f"Set variable '{key}' = {value} (type: {type(value).__name__})",
                                )
                            self.event_manager.emit_log(
                                "info",
                                f"Successfully applied {len(variables)} variables to execution context",
                            )
                        else:
                            self.event_manager.emit_log(
                                "warning", "Action executor does not have variable_context"
                            )
                    except Exception as e:
                        self.event_manager.emit_log("error", f"Failed to apply variables: {e}")
                        self.event_manager.emit_log(
                            "debug", f"Traceback: {traceback.format_exc()}"
                        )  # noqa: F823
                else:
                    self.event_manager.emit_log(
                        "warning",
                        "Cannot apply variables: executor_core or action_executor not initialized",
                    )

            # Get the workflow ID to execute (first workflow if not specified)
            workflow_id = None
            if self.config and "workflows" in self.config:
                workflows = self.config.get("workflows", [])
                if workflows:
                    workflow_id = workflows[0].get("id")

            if not workflow_id:
                return {"success": False, "error": "No workflow found in configuration"}

            # Start execution
            self.event_manager.emit_log(
                "info", f"Starting remote workflow execution: {workflow_id}"
            )
            start_success = self.start_execution(workflow_id)

            if start_success:
                # Send execution started event through WebSocket
                if self.websocket_handler and self.websocket_handler.is_connected:
                    execution_event = {
                        "type": "execution_started",
                        "data": {
                            "execution_id": execution_id,
                            "workflow_id": workflow_id,
                            "workflow_name": workflow.get("name", "Unknown"),
                        },
                    }
                    self.websocket_handler.send_message(json.dumps(execution_event))

                return {
                    "success": True,
                    "execution_id": execution_id,
                    "workflow_id": workflow_id,
                    "message": "Workflow execution started",
                }
            else:
                return {"success": False, "error": "Failed to start workflow execution"}

        except Exception as e:
            print(
                f"[error   ] EXECUTOR: Failed to execute workflow: {e}",
                file=sys.stderr,
                flush=True,
            )

            print(
                f"[error   ] EXECUTOR: Traceback: {traceback.format_exc()}",
                file=sys.stderr,
                flush=True,
            )
            self.event_manager.emit_log("error", f"Failed to execute workflow: {e}")
            return {"success": False, "error": str(e)}

    def __del__(self):
        """Clean up resources on exit."""
        # Stop capture manager
        if (
            hasattr(self, "capture_manager")
            and self.capture_manager
            and self.capture_manager.manual_click_listener
        ):
            with contextlib.suppress(Exception):
                self.capture_manager.manual_click_listener.cleanup()

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

            # Don't log high-frequency commands to avoid flooding the logs
            if cmd_name not in ("ping", "status"):
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
