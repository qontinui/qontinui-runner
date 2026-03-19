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
    stream=sys.stderr,
    level=logging.DEBUG,
    format="%(asctime)s [%(levelname)s] %(message)s",
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
from training_export import TrainingExportCoordinator  # noqa: E402
from websocket_handler import WebSocketHandler  # noqa: E402

# TestResultsHandler module may not exist; import gracefully
try:
    from test_results_handler import TestResultsHandler  # noqa: E402
except ImportError:
    TestResultsHandler = None  # type: ignore[assignment, misc]

# UI Bridge explorer - lightweight, doesn't need ML stack
from services.ui_bridge_explorer_service import UIBridgeExplorerService  # noqa: E402

# Services that may require heavy ML dependencies (torch, easyocr, etc.)
# Import them gracefully so the executor can start without the full ML stack.
_ML_SERVICES_AVAILABLE = True
try:
    from gui_automation import GUIAutomation  # noqa: E402
    from services.accessibility_capture_service import (
        AccessibilityCaptureService,
    )  # noqa: E402
    from services.ai_builder_generator import AiBuilderGeneratorService  # noqa: E402
    from services.ai_shell_command_generator import (
        AiShellCommandGeneratorService,
    )  # noqa: E402
    from services.ai_test_generator import AiTestGeneratorService  # noqa: E402
    from services.input_monitor_service import InputMonitorService  # noqa: E402
    from services.integration_testing_service import (
        IntegrationTestingService,
    )  # noqa: E402
    from services.model_manager import get_model_manager  # noqa: E402
    from services.pattern_matching_service import (
        get_pattern_matching_service,
    )  # noqa: E402
    from services.playwright_collector_service import (
        PlaywrightCollectorService,
    )  # noqa: E402
    from services.screenshot_service import ScreenshotService  # noqa: E402
    from services.test_analysis_service import TestAnalysisService  # noqa: E402
    from services.uitars_extraction_service import (  # noqa: E402
        UITarsExtractionService,
        get_uitars_extraction_service,
    )
    from services.unified_data_collector import UnifiedDataCollector  # noqa: E402
    from services.vision_extraction_service import VisionExtractionService  # noqa: E402
    from services.web_extraction_service import WebExtractionService  # noqa: E402
except ImportError as _ml_import_err:
    _ML_SERVICES_AVAILABLE = False
    _ml_import_error_msg = f"{type(_ml_import_err).__name__}: {_ml_import_err}"
    logger.warning(f"ML services not available (non-fatal): {_ml_import_error_msg}")
    # Provide None placeholders for unavailable services
    GUIAutomation = None  # type: ignore[assignment, misc]
    AccessibilityCaptureService = None  # type: ignore[assignment, misc]
    AiBuilderGeneratorService = None  # type: ignore[assignment, misc]
    AiShellCommandGeneratorService = None  # type: ignore[assignment, misc]
    AiTestGeneratorService = None  # type: ignore[assignment, misc]
    InputMonitorService = None  # type: ignore[assignment, misc]
    IntegrationTestingService = None  # type: ignore[assignment, misc]
    get_model_manager = None  # type: ignore[assignment, misc]
    get_pattern_matching_service = None  # type: ignore[assignment, misc]
    PlaywrightCollectorService = None  # type: ignore[assignment, misc]
    ScreenshotService = None  # type: ignore[assignment, misc]
    TestAnalysisService = None  # type: ignore[assignment, misc]
    UITarsExtractionService = None  # type: ignore[assignment, misc]
    get_uitars_extraction_service = None  # type: ignore[assignment, misc]
    UnifiedDataCollector = None  # type: ignore[assignment, misc]
    VisionExtractionService = None  # type: ignore[assignment, misc]
    WebExtractionService = None  # type: ignore[assignment, misc]

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

    def __init__(self) -> None:
        """Initialize executor and all modules."""
        self.config: Any = None
        self.is_running = False
        self._is_paused = False
        self._pause_event = threading.Event()
        self._pause_event.set()  # Start in unpaused state (event is set = not waiting)
        self._navigation_sequence = 0
        self.target_monitor: int | None = None  # Monitor index for execution

        # Initialize EventManager first (other modules depend on it)
        self.event_manager = EventManager()

        # Initialize WebSocketHandler with command handler
        self.websocket_handler = WebSocketHandler(
            emit_log_fn=self.event_manager.emit_log,
            on_command_fn=self._handle_websocket_command,
        )

        # Initialize TestResultsHandler for QA dashboard submission
        if TestResultsHandler is not None:
            self.test_results_handler = TestResultsHandler(
                emit_log_fn=self.event_manager.emit_log
            )
        else:
            self.test_results_handler = None

        # Initialize CaptureManager
        self.capture_manager = CaptureManager(
            emit_log_fn=self.event_manager.emit_log,
            emit_event_fn=self.event_manager.emit_event,
        )

        # Initialize TrainingExportCoordinator
        self.training_export = TrainingExportCoordinator(
            emit_log_fn=self.event_manager.emit_log
        )

        # Initialize ExecutorCore
        self.executor_core = ExecutorCore(
            emit_log_fn=self.event_manager.emit_log,
            emit_event_fn=self.event_manager.emit_event,
        )

        # Execution tree for hierarchical tracking
        self.execution_tree = ExecutionTree()

        # Unified data collector (initialized after config load)
        self.unified_data_collector: Any = None
        self.screenshot_service: Any = None

        # InputMonitorService for validation capture (initialized on demand)
        self.input_monitor_service: Any = None
        # Flag to enable input capture during workflow execution
        self.capture_input_for_validation = False
        self._input_capture_session_id: str | None = None

        # Interaction Recording state (combined video + input capture for State Machine creation)
        self._interaction_recording_active = False
        self._interaction_session_id: str | None = None
        self._interaction_start_time: float | None = None
        self._interaction_video_path: str | None = None
        self._interaction_fps: int = 30

        # Click Capture state (for click-to-template extraction)
        self._click_capture_active = False
        self._click_capture_session_id: str | None = None
        self._click_capture_start_time: float | None = None
        self._click_capture_output_dir: str | None = None
        self._click_capture_application_hint: str | None = None

        # GUIAutomation (initialized after config load)
        self.gui_automation: Any = None

        # Web extraction service (lazy-loaded when needed)
        self._web_extraction_service: Any = None

        # Vision extraction service (lazy-loaded when needed)
        self._vision_extraction_service: Any = None

        # Playwright collector service (lazy-loaded when needed)
        self._playwright_collector_service: Any = None

        # Test analysis service for AI-powered test generation (lazy-loaded when needed)
        self._test_analysis_service: Any = None

        # AI test generator service (lazy-loaded when needed)
        self._ai_test_generator_service: Any = None

        # AI shell command generator service (lazy-loaded when needed)
        self._ai_shell_command_generator_service: Any = None

        # AI builder generator service (lazy-loaded when needed)
        self._ai_builder_generator_service: Any = None

        # Integration testing service (lazy-loaded when needed)
        self._integration_testing_service: Any = None

        # Accessibility capture service (lazy-loaded when needed)
        self._accessibility_capture_service: Any = None

        # UI-TARS extraction service (lazy-loaded when needed)
        self._uitars_extraction_service: UITarsExtractionService | None = None

        # UI Bridge explorer service (lazy-loaded when needed)
        self._ui_bridge_explorer_service: UIBridgeExplorerService | None = None

        # UI Bridge Runtime state machine (loaded via load_state_machine command)
        self._ui_bridge_runtime: Any = None

        # State machine persistence (lazy-initialized)
        self._sm_persistence: Any = None
        self._element_resolver: Any = None

        # Dedicated event loop for async operations (avoids conflicts with WebSocket thread's loop)
        # Runs in a background thread to allow run_coroutine_threadsafe()
        self._async_loop: Any = None
        self._async_thread: threading.Thread | None = None

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
            logger.debug(
                f"[INIT] Event registry has_listeners: {event_registry.has_listeners}"
            )
            logger.debug("[INIT] EventTranslator initialized and callbacks registered")

        logger.info(
            f"QontinuiExecutor initialized (library_available={QONTINUI_AVAILABLE})"
        )

        # Auto-reload persisted state machine (non-fatal if it fails)
        with contextlib.suppress(Exception):
            self._try_reload_state_machine()

    def _get_sm_persistence(self) -> Any:
        """Lazy-initialize StateMachinePersistence."""
        if self._sm_persistence is None:
            from state_machine_persistence import (
                StateMachinePersistence,
                get_app_data_dir,
            )

            db_path = get_app_data_dir() / "state_machine.db"
            self._sm_persistence = StateMachinePersistence(db_path)
        return self._sm_persistence

    @staticmethod
    def _get_runner_api_port() -> int:
        """Get the runner API port from QONTINUI_PORT env var, defaulting to 9876."""
        return int(os.environ.get("QONTINUI_PORT", "9876"))

    @staticmethod
    def _get_runner_api_base() -> str:
        """Get the runner API base URL using 127.0.0.1 (more robust than localhost)."""
        port = int(os.environ.get("QONTINUI_PORT", "9876"))
        return f"http://127.0.0.1:{port}"

    def _try_reload_state_machine(self) -> None:
        """Attempt to reload a persisted state machine on startup."""
        persistence = self._get_sm_persistence()
        if not persistence.has_data():
            return
        config_data = persistence.load_config()
        if not config_data:
            return

        from element_resolver import ElementResolver
        from ui_bridge_http_client import ResolvingUIBridgeClient, UIBridgeHTTPClient

        from qontinui.state_machine.ui_bridge_runtime import UIBridgeRuntime

        inner_client = UIBridgeHTTPClient(self._get_runner_api_base())
        resolver = ElementResolver(persistence)
        client = ResolvingUIBridgeClient(inner_client, resolver)

        runtime = UIBridgeRuntime.from_dict(config_data, client)
        self._ui_bridge_runtime = runtime
        self._element_resolver = resolver
        self.event_manager.emit_log("info", "Auto-reloaded persisted state machine")

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

        print(
            f"[FWD_TO_WS] Called: event_type={event_type}", file=sys.stderr, flush=True
        )
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
            self.screenshot_service = ScreenshotService(
                storage_dir=run_dir, enabled=True
            )
            self.event_manager.emit_log(
                "info", f"ScreenshotService initialized: {run_dir}"
            )

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
                            pattern_name=match_summary.get(
                                "image_id"
                            ),  # Using image_id as name
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
                            duration_ms=(
                                int(record.duration_ms) if record.duration_ms else None
                            ),
                            result_data={
                                "config": record.config,
                                "clicked_location": record.clicked_location,
                                "transition_data": record.transition_data,
                            },
                        )
                    except Exception as e:
                        self.event_manager.emit_log(
                            "debug",
                            f"Failed to report action for historical indexing: {e}",
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
            self.event_manager.emit_log(
                "error", f"Failed to initialize unified data services: {e}"
            )
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

        debug_log_path = os.path.join(
            tempfile.gettempdir(), "qontinui_load_config_debug.log"
        )
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
                f.write(
                    f"executor_core.action_executor: {self.executor_core.action_executor}\n"
                )
                f.write(
                    f"executor_core.state_executor: {self.executor_core.state_executor}\n"
                )
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
            # Set pause event for synchronization
            self.gui_automation.set_pause_event(self._pause_event)

            # Inject self as workflow executor for navigation
            if QONTINUI_AVAILABLE:
                navigation_api.set_workflow_executor(self)
                self.event_manager.emit_log(
                    "info", "Runner injected as workflow_executor"
                )

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
                self.input_monitor_service = InputMonitorService(
                    storage_dir=dev_logs_dir
                )
                self.event_manager.emit_log(
                    "info", f"InputMonitorService initialized: {dev_logs_dir}"
                )

            self.input_monitor_service.start_monitoring(session_id=session_id, fps=30)
            self._input_capture_session_id = session_id
            self.event_manager.emit_log(
                "info",
                f"Input capture started for execution validation: session={session_id}",
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
        import asyncio

        if not self.gui_automation:
            return {"success": False, "error": "GUI automation not initialized"}

        try:
            # Run async workflow in sync context
            loop = self._get_or_create_async_loop()
            future = asyncio.run_coroutine_threadsafe(
                self.gui_automation.execute_workflow(workflow_id, transition_context),
                loop,
            )
            success = future.result(timeout=600)  # 10 minute timeout for workflows
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
                f.write(
                    f"[{timestamp}] [PYTHON_EXECUTOR] Set self.target_monitor = {monitor}\n"
                )
        except Exception:
            pass
        if monitor is not None:
            self.event_manager.emit_log(
                "info", f"[PYTHON_EXECUTOR] Using monitor index: {monitor}"
            )
            # Apply monitor setting to FrameworkSettings so qontinui core uses this monitor
            if QONTINUI_AVAILABLE:
                try:
                    settings = get_settings()
                    settings.monitor.default_screen_index = monitor
                    self.event_manager.emit_log(
                        "debug",
                        f"Set FrameworkSettings.monitor.default_screen_index = {monitor}",
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
                                os.path.join(
                                    tempfile.gettempdir(), "qontinui_monitor_debug.log"
                                ),
                                "a",
                                encoding="utf-8",
                            ) as f:
                                timestamp = datetime.now().strftime(
                                    "%Y-%m-%d %H:%M:%S.%f"
                                )[:-3]
                                f.write(
                                    f"[{timestamp}] [PYTHON_EXECUTOR] Set target monitor: {monitor}\n"
                                )
                        except Exception:
                            pass
                    else:
                        # Debug: Log why we couldn't set monitor
                        try:
                            with open(
                                os.path.join(
                                    tempfile.gettempdir(), "qontinui_monitor_debug.log"
                                ),
                                "a",
                                encoding="utf-8",
                            ) as f:
                                timestamp = datetime.now().strftime(
                                    "%Y-%m-%d %H:%M:%S.%f"
                                )[:-3]
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
        ws_config = self.websocket_handler.ws_config
        ws_config_exists = ws_config is not None
        ws_config_enabled = ws_config.enabled if ws_config is not None else False
        self.event_manager.emit_log(
            "debug",
            f"[WS_SESSION_CHECK] is_enabled={ws_is_enabled}, ws_config_exists={ws_config_exists}, ws_config.enabled={ws_config_enabled}, ws_enabled={self.websocket_handler.ws_enabled}",
        )
        if ws_is_enabled or (ws_config_exists and ws_config_enabled):
            self.event_manager.emit_log(
                "info", "[WS_SESSION_CHECK] Starting WebSocket session..."
            )
            result = self.websocket_handler.start_session(config_snapshot=self.config)
            self.event_manager.emit_log(
                "info", f"[WS_SESSION_CHECK] start_session result: {result}"
            )
        else:
            self.event_manager.emit_log(
                "warning",
                "[WS_SESSION_CHECK] WebSocket session NOT started - conditions not met",
            )

        # Start execution in background thread
        thread = threading.Thread(
            target=self._run_workflow, args=(workflow_id,), daemon=True
        )
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
            self.event_manager.emit_log(
                "info", f"Starting workflow execution: {workflow_id}"
            )

            # Reset navigation state
            if self.gui_automation:
                self.gui_automation.reset_navigation_state()

            # Get workflow config for test results
            workflow = (
                self.executor_core.workflows.get(workflow_id)
                if self.executor_core
                else None
            )
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

            # Execute workflow (async method called from sync context)
            import asyncio

            loop = self._get_or_create_async_loop()
            future = asyncio.run_coroutine_threadsafe(
                self.gui_automation.execute_workflow(workflow_id), loop
            )
            success = future.result(timeout=600)  # 10 minute timeout for workflows

            self.event_manager.emit_event(
                EventType.EXECUTION_COMPLETED,
                {
                    "success": success,
                    "workflow_id": workflow_id,
                },
            )

            # End WebSocket session
            if self.websocket_handler.is_enabled():
                self.websocket_handler.end_session(
                    status="completed" if success else "failed"
                )

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

            # Reset pause state
            self._is_paused = False
            self._pause_event.set()  # Unblock any waiting threads

            # Stop input capture for coordinate validation
            self._stop_input_capture_for_execution()

            if self.gui_automation:
                self.gui_automation.set_running(False)
                self.gui_automation.set_paused(False)

            self.event_manager.emit_event(
                EventType.EXECUTION_COMPLETED,
                {"success": False, "reason": "User stopped"},
            )

            # Export training data if enabled
            if self.training_export.is_enabled():
                self.event_manager.emit_log(
                    "info", "Exporting training data on stop..."
                )
                self.training_export.export_data()

    def pause_execution(self):
        """Pause the current execution."""
        if self.is_running and not self._is_paused:
            self.event_manager.emit_log("info", "Pausing execution...")
            self._is_paused = True
            self._pause_event.clear()  # Block waiting threads

            if self.gui_automation:
                self.gui_automation.set_paused(True)

            self.event_manager.emit_event(EventType.EXECUTION_PAUSED, {"paused": True})

    def resume_execution(self):
        """Resume a paused execution."""
        if self.is_running and self._is_paused:
            self.event_manager.emit_log("info", "Resuming execution...")
            self._is_paused = False
            self._pause_event.set()  # Unblock waiting threads

            if self.gui_automation:
                self.gui_automation.set_paused(False)

            self.event_manager.emit_event(
                EventType.EXECUTION_RESUMED, {"paused": False}
            )

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

            # IMPORTANT: Set is_running=True so navigation workflows can execute actions
            # Without this, the gui_automation._execute_workflow_internal method will skip
            # all actions because it checks `if not self.is_running: break`
            was_running = self.is_running
            self.is_running = True
            self.gui_automation.set_running(True)

            # Navigate
            try:
                result = navigation_api.open_state(target_state_id)

                # Update node status
                success = (
                    result.get("success", False) if isinstance(result, dict) else result
                )
                nav_node.status = "completed" if success else "failed"
                if not success:
                    nav_node.error = (
                        result.get("error", "Navigation failed")
                        if isinstance(result, dict)
                        else "Navigation failed"
                    )

                # Emit completion
                self.event_manager.emit_tree_event(
                    "workflow_completed" if success else "workflow_failed",
                    nav_node,
                    None,
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
            finally:
                # Restore is_running state after navigation
                if not was_running:
                    self.is_running = False
                    self.gui_automation.set_running(False)
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
                    "[error   ] EXECUTOR: No command name in message",
                    file=sys.stderr,
                    flush=True,
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
            print(
                f"[info    ] EXECUTOR: Command result: {result}",
                file=sys.stderr,
                flush=True,
            )

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
            self.event_manager.emit_log(
                "info", f"handle_command: received '{cmd_type}'"
            )

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

        elif cmd_type == "pause":
            self.pause_execution()
            return {"success": True}

        elif cmd_type == "resume":
            self.resume_execution()
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
                import asyncio

                # Set monitor if provided
                if self.executor_core.state_executor and monitor_index is not None:
                    self.executor_core.state_executor.set_monitor(monitor_index)

                # Execute the action (async method called from sync context)
                loop = self._get_or_create_async_loop()
                future = asyncio.run_coroutine_threadsafe(
                    self.gui_automation.execute_action(action_data), loop
                )
                success = future.result(
                    timeout=120
                )  # 2 minute timeout for single actions

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
            return {
                "success": True,
                "is_running": self.capture_manager.is_manual_capture_running(),
            }

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
            runner_port = params.get("runner_port")
            print(
                f"[info    ] WS_DEBUG: enabled={enabled}, api_url={api_url}, project_id={project_id}, runner_name={runner_name}, runner_port={runner_port}, token_len={len(token) if token else 0}",
                file=sys.stderr,
                flush=True,
            )
            self.event_manager.emit_log(
                "info",
                f"[WS_CONFIGURE] enabled={enabled}, api_url={api_url}, project_id={project_id}, runner_name={runner_name}, runner_port={runner_port}, token_len={len(token) if token else 0}",
            )
            success = self.websocket_handler.configure(
                enabled, api_url, token, project_id, runner_name, runner_port
            )
            print(
                f"[info    ] WS_DEBUG: configure result: success={success}",
                file=sys.stderr,
                flush=True,
            )
            self.event_manager.emit_log(
                "info", f"[WS_CONFIGURE] Result: success={success}"
            )
            return {"success": success}

        elif cmd_type == "ws_connect":
            import sys
            import threading

            print(
                "[info    ] WS_DEBUG: ws_connect received!", file=sys.stderr, flush=True
            )

            # Run connection in background thread to avoid blocking the command loop.
            # A blocking ws_connect prevents the executor from responding to health
            # check pings, causing Rust to declare the executor unresponsive.
            def _connect_in_background():
                success = self.websocket_handler.connect()
                print(
                    f"[info    ] WS_DEBUG: ws_connect result: success={success}",
                    file=sys.stderr,
                    flush=True,
                )
                self.event_manager.emit_log(
                    "info" if success else "error",
                    f"[WS_CONNECT] Background connect result: success={success}",
                )

            thread = threading.Thread(target=_connect_in_background, daemon=True)
            thread.start()
            return {"success": True}

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
            # Support both "target_state_id" (from Rust action_service) and "state_id" (legacy)
            state_id = params.get("target_state_id") or params.get("state_id")
            return self.navigate_to_state(state_id)

        # Web extraction commands
        elif cmd_type == "start_web_extraction":
            return self._handle_start_web_extraction(params)

        elif cmd_type == "stop_web_extraction":
            return self._handle_stop_web_extraction()

        elif cmd_type == "get_extraction_status":
            return self._handle_get_extraction_status()

        # Playwright State Collector commands
        elif cmd_type == "start_playwright_collection":
            return self._handle_start_playwright_collection(params)

        elif cmd_type == "get_playwright_collection_status":
            return self._handle_get_playwright_collection_status(params)

        elif cmd_type == "get_playwright_collection_results":
            return self._handle_get_playwright_collection_results(params)

        elif cmd_type == "stop_playwright_collection":
            return self._handle_stop_playwright_collection()

        # UI-TARS extraction commands
        elif cmd_type == "start_uitars_extraction":
            return self._handle_start_uitars_extraction(params)

        elif cmd_type == "stop_uitars_extraction":
            return self._handle_stop_uitars_extraction()

        elif cmd_type == "get_uitars_extraction_status":
            return self._handle_get_uitars_extraction_status()

        elif cmd_type == "get_uitars_extraction_results":
            return self._handle_get_uitars_extraction_results()

        # Vision extraction commands
        elif cmd_type == "run_vision_extraction":
            return self._handle_run_vision_extraction(params)

        # Remote workflow execution from web app
        elif cmd_type == "execute_workflow":
            return self._handle_execute_workflow(params)

        # Screenshot capture command (for direct capture via Python)
        elif cmd_type == "capture_screenshot":
            return self._handle_capture_screenshot(params)

        # Get available monitors
        elif cmd_type == "get_monitors":
            return self._handle_get_monitors()

        # SAM3 segmentation command
        elif cmd_type == "segment_screenshot":
            return self._handle_segment_screenshot(params)

        # Verification Agent commands
        elif cmd_type == "detect_current_states":
            return self._handle_detect_current_states(params)

        elif cmd_type == "verify_elements":
            return self._handle_verify_elements(params)

        elif cmd_type == "verification_capture_screenshot":
            return self._handle_verification_capture_screenshot(params)

        # Flakiness-aware execution commands
        elif cmd_type == "get_flakiness_options":
            return self._handle_get_flakiness_options(params)

        # Interaction Recording commands (video + input capture for State Machine creation)
        elif cmd_type == "start_interaction_recording":
            return self._handle_start_interaction_recording(params)

        elif cmd_type == "stop_interaction_recording":
            return self._handle_stop_interaction_recording()

        elif cmd_type == "get_interaction_recording_status":
            return self._handle_get_interaction_recording_status()

        # Page analysis commands for AI-powered test generation
        elif cmd_type == "analyze_page_playwright":
            return self._handle_analyze_page_playwright(params)

        elif cmd_type == "analyze_page_playwright_script":
            return self._handle_analyze_page_playwright_script(params)

        elif cmd_type == "analyze_page_vision":
            return self._handle_analyze_page_vision(params)

        elif cmd_type == "generate_test_with_ai":
            return self._handle_generate_test_with_ai(params)

        # AI shell command generation
        elif cmd_type == "generate_shell_command_with_ai":
            return self._handle_generate_shell_command_with_ai(params)

        # AI builder generation commands
        elif cmd_type == "generate_context_with_ai":
            return self._handle_generate_context_with_ai(params)

        elif cmd_type == "generate_api_request_with_ai":
            return self._handle_generate_api_request_with_ai(params)

        elif cmd_type == "generate_task_prompt_with_ai":
            return self._handle_generate_task_prompt_with_ai(params)

        elif cmd_type == "suggest_exploration_strategy_with_ai":
            return self._handle_suggest_exploration_strategy_with_ai(params)

        elif cmd_type == "generate_test_and_agentic_step":
            return self._handle_generate_test_and_agentic_step(params)

        elif cmd_type == "explore_flow_step":
            return self._handle_explore_flow_step(params)

        # Pattern matching commands
        elif cmd_type == "pattern_find":
            return self._handle_pattern_find(params)

        elif cmd_type == "pattern_find_all":
            return self._handle_pattern_find_all(params)

        # Integration testing commands
        elif cmd_type == "testing_get_states":
            return self._handle_testing_get_states()

        elif cmd_type == "testing_get_transitions":
            return self._handle_testing_get_transitions()

        elif cmd_type == "testing_find_path":
            return self._handle_testing_find_path(params)

        elif cmd_type == "testing_traverse_to_state":
            return self._handle_testing_traverse_to_state(params)

        elif cmd_type == "testing_get_active_states":
            return self._handle_testing_get_active_states()

        elif cmd_type == "testing_set_mock_mode":
            return self._handle_testing_set_mock_mode(params)

        elif cmd_type == "testing_mock_click":
            return self._handle_testing_mock_click(params)

        elif cmd_type == "testing_mock_type":
            return self._handle_testing_mock_type(params)

        elif cmd_type == "testing_mock_screenshot":
            return self._handle_testing_mock_screenshot(params)

        elif cmd_type == "testing_get_mocked_actions":
            return self._handle_testing_get_mocked_actions()

        elif cmd_type == "testing_clear_mocked_actions":
            return self._handle_testing_clear_mocked_actions()

        elif cmd_type == "testing_start_run":
            return self._handle_testing_start_run(params)

        elif cmd_type == "testing_run_assertion":
            return self._handle_testing_run_assertion(params)

        elif cmd_type == "testing_run_test_case":
            return self._handle_testing_run_test_case(params)

        elif cmd_type == "testing_end_run":
            return self._handle_testing_end_run()

        elif cmd_type == "testing_get_run":
            return self._handle_testing_get_run(params)

        elif cmd_type == "testing_get_status":
            return self._handle_testing_get_status(params)

        elif cmd_type == "testing_get_results":
            return self._handle_testing_get_results(params)

        elif cmd_type == "testing_list_runs":
            return self._handle_testing_list_runs(params)

        # Model management commands
        elif cmd_type == "models_list":
            return self._handle_models_list()

        elif cmd_type == "models_download":
            return self._handle_models_download(params)

        elif cmd_type == "models_delete":
            return self._handle_models_delete(params)

        elif cmd_type == "models_status":
            return self._handle_models_status(params)

        elif cmd_type == "models_disk_usage":
            return self._handle_models_disk_usage()

        # Accessibility capture commands
        elif cmd_type == "capture_accessibility":
            return self._handle_capture_accessibility(params)

        elif cmd_type == "click_ref":
            return self._handle_click_ref(params)

        elif cmd_type == "fill_ref":
            return self._handle_fill_ref(params)

        elif cmd_type == "focus_ref":
            return self._handle_focus_ref(params)

        elif cmd_type == "get_ref":
            return self._handle_get_ref(params)

        elif cmd_type == "get_accessibility_snapshot":
            return self._handle_get_accessibility_snapshot()

        elif cmd_type == "get_accessibility_ai_context":
            return self._handle_get_accessibility_ai_context(params)

        elif cmd_type == "find_accessibility_elements":
            return self._handle_find_accessibility_elements(params)

        elif cmd_type == "disconnect_accessibility":
            return self._handle_disconnect_accessibility()

        elif cmd_type == "scan_cdp_ports":
            return self._handle_scan_cdp_ports(params)

        elif cmd_type == "list_browser_targets":
            return self._handle_list_browser_targets(params)

        elif cmd_type == "auto_connect_accessibility":
            return self._handle_auto_connect_accessibility(params)

        # AWAS (AI Web Action Standard) commands
        elif cmd_type == "awas_discover":
            return self._handle_awas_discover(params)

        elif cmd_type == "awas_execute":
            return self._handle_awas_execute(params)

        elif cmd_type == "awas_check_support":
            return self._handle_awas_check_support(params)

        elif cmd_type == "awas_list_actions":
            return self._handle_awas_list_actions(params)

        elif cmd_type == "awas_extract_elements":
            return self._handle_awas_extract_elements(params)

        # UI Bridge exploration commands
        elif cmd_type == "start_ui_bridge_exploration":
            return self._handle_start_ui_bridge_exploration(params)
        elif cmd_type == "get_ui_bridge_exploration_status":
            return self._handle_get_ui_bridge_exploration_status(params)
        elif cmd_type == "get_ui_bridge_exploration_results":
            return self._handle_get_ui_bridge_exploration_results(params)
        elif cmd_type == "stop_ui_bridge_exploration":
            return self._handle_stop_ui_bridge_exploration(params)

        # UI Bridge state discovery from render logs
        elif cmd_type == "discover_states_from_renders":
            return self._handle_discover_states_from_renders(params)

        # UI Bridge state discovery from fingerprint co-occurrence data
        elif cmd_type == "discover_states_from_fingerprints":
            return self._handle_discover_states_from_fingerprints(params)

        # UI Bridge automatic exploration
        elif cmd_type == "run_ui_bridge_exploration":
            return self._handle_run_ui_bridge_exploration(params)

        # Click-to-Template capture commands
        elif cmd_type == "start_click_capture":
            return self._handle_start_click_capture(params)

        elif cmd_type == "stop_click_capture":
            return self._handle_stop_click_capture(params)

        elif cmd_type == "get_click_capture_status":
            return self._handle_get_click_capture_status()

        elif cmd_type == "process_click_capture":
            return self._handle_process_click_capture(params)

        elif cmd_type == "generate_state_machine":
            return self._handle_generate_state_machine(params)

        # UI Bridge State Machine commands
        elif cmd_type == "load_state_machine":
            return self._handle_load_state_machine(params)
        elif cmd_type == "get_state_machine_status":
            return self._handle_get_state_machine_status()
        elif cmd_type == "sm_execute_transition":
            return self._handle_sm_execute_transition(params)
        elif cmd_type == "sm_navigate_to_states":
            return self._handle_sm_navigate_to_states(params)
        elif cmd_type == "sm_get_active_states":
            return self._handle_sm_get_active_states()
        elif cmd_type == "sm_get_available_transitions":
            return self._handle_sm_get_available_transitions()
        elif cmd_type == "clear_state_machine":
            return self._handle_clear_state_machine()

        # GUI Config Pipeline commands
        elif cmd_type == "gui_config_capture_elements":
            return self._handle_gui_config_capture_elements(params)
        elif cmd_type == "gui_config_build":
            return self._handle_gui_config_build(params)
        elif cmd_type == "gui_config_capture_multi_state":
            return self._handle_gui_config_capture_multi_state(params)

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
                            image_base64_out = base64.b64encode(
                                buffer.getvalue()
                            ).decode("utf-8")

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

    def _get_vision_extraction_service(self) -> VisionExtractionService:
        """Get or create the vision extraction service."""
        if self._vision_extraction_service is None:
            self._vision_extraction_service = VisionExtractionService(
                event_manager=self.event_manager,
            )
        return self._vision_extraction_service  # type: ignore[no-any-return]

    def _get_test_analysis_service(self) -> TestAnalysisService:
        """Get or create the test analysis service for AI-powered test generation."""
        if self._test_analysis_service is None:
            self._test_analysis_service = TestAnalysisService(
                event_manager=self.event_manager,
            )
        return self._test_analysis_service  # type: ignore[no-any-return]

    def _get_accessibility_capture_service(self) -> AccessibilityCaptureService:
        """Get or create the accessibility capture service."""
        if self._accessibility_capture_service is None:
            self._accessibility_capture_service = AccessibilityCaptureService()
        return self._accessibility_capture_service  # type: ignore[no-any-return]

    def _handle_analyze_page_playwright(self, params: dict[str, Any]) -> dict[str, Any]:
        """
        Analyze page via Playwright CDP for AI test generation.

        Connects to an existing browser via Chrome DevTools Protocol,
        captures a screenshot, and extracts DOM elements with their
        bounding boxes, text content, and CSS selectors.

        Args:
            params: Configuration with:
                - cdp_port: CDP port number (default: 9222)

        Returns:
            Dict with:
                - success: Whether analysis succeeded
                - analysis: PageAnalysis data if success
                - error: Error message if failed
        """
        import asyncio
        import sys

        cdp_port = params.get("cdp_port", 9222)

        print(
            f"[info    ] EXECUTOR: _handle_analyze_page_playwright called with cdp_port={cdp_port}",
            file=sys.stderr,
            flush=True,
        )

        try:
            service = self._get_test_analysis_service()

            # Run async analysis using the async loop
            loop = self._get_or_create_async_loop()
            future = asyncio.run_coroutine_threadsafe(
                service.analyze_via_playwright(cdp_port=cdp_port),
                loop,
            )
            result = future.result(timeout=60)  # 60 second timeout for CDP connection

            if result.get("success"):
                return {
                    "success": True,
                    "analysis": result.get("data"),  # Return the actual analysis data
                }
            else:
                return {
                    "success": False,
                    "error": result.get(
                        "error", "Unknown error during Playwright analysis"
                    ),
                }

        except TimeoutError:
            return {
                "success": False,
                "error": f"Timeout connecting to browser via CDP on port {cdp_port}. Make sure a browser is running with --remote-debugging-port={cdp_port}",
            }
        except Exception as e:
            import traceback

            return {
                "success": False,
                "error": f"Playwright analysis failed: {e}",
                "traceback": traceback.format_exc(),
            }

    def _handle_analyze_page_playwright_script(
        self, params: dict[str, Any]
    ) -> dict[str, Any]:
        """
        Analyze page by running a Playwright script.

        Executes the provided Playwright script to navigate to a page,
        then captures DOM elements with bounding boxes, text content,
        and CSS selectors.

        Args:
            params: Configuration with:
                - script: Playwright TypeScript/JavaScript code

        Returns:
            Dict with:
                - success: Whether analysis succeeded
                - analysis: PageAnalysis data if success
                - error: Error message if failed
        """
        import asyncio
        import sys

        script = params.get("script", "")

        print(
            f"[info    ] EXECUTOR: _handle_analyze_page_playwright_script called with script ({len(script)} chars)",
            file=sys.stderr,
            flush=True,
        )

        if not script.strip():
            return {
                "success": False,
                "error": "Playwright script is required",
            }

        try:
            service = self._get_test_analysis_service()

            # Run async analysis using the async loop
            loop = self._get_or_create_async_loop()
            future = asyncio.run_coroutine_threadsafe(
                service.analyze_via_playwright_script(script=script),
                loop,
            )
            result = future.result(timeout=120)  # 2 minute timeout for script execution

            if result.get("success"):
                return {
                    "success": True,
                    "analysis": result.get("data"),
                }
            else:
                return {
                    "success": False,
                    "error": result.get(
                        "error", "Unknown error during Playwright script analysis"
                    ),
                }

        except TimeoutError:
            return {
                "success": False,
                "error": "Timeout running Playwright script. Check for infinite loops or slow navigation.",
            }
        except Exception as e:
            import traceback

            return {
                "success": False,
                "error": f"Playwright script analysis failed: {e}",
                "traceback": traceback.format_exc(),
            }

    def _handle_analyze_page_vision(self, params: dict[str, Any]) -> dict[str, Any]:
        """
        Analyze page via Qontinui Vision for AI test generation.

        Captures a screenshot from the specified monitor and runs
        OCR, edge detection, and SAM3 segmentation to detect UI elements.

        Args:
            params: Configuration with:
                - monitor_index: Monitor index to capture (default: 0)
                - screenshot_base64: Optional pre-captured screenshot

        Returns:
            Dict with:
                - success: Whether analysis succeeded
                - analysis: PageAnalysis data if success
                - error: Error message if failed
        """
        import sys

        monitor_index = params.get("monitor_index", 0)
        screenshot_base64 = params.get("screenshot_base64")

        print(
            f"[info    ] EXECUTOR: _handle_analyze_page_vision called with monitor_index={monitor_index}",
            file=sys.stderr,
            flush=True,
        )

        try:
            service = self._get_test_analysis_service()

            # Vision analysis is synchronous
            result = service.analyze_via_vision(
                screenshot_base64=screenshot_base64,
                monitor_index=monitor_index,
            )

            if result.get("success"):
                return {
                    "success": True,
                    "analysis": result.get("data"),  # Return the actual analysis data
                }
            else:
                return {
                    "success": False,
                    "error": result.get(
                        "error", "Unknown error during Vision analysis"
                    ),
                }

        except Exception as e:
            import traceback

            return {
                "success": False,
                "error": f"Vision analysis failed: {e}",
                "traceback": traceback.format_exc(),
            }

    def _get_ai_test_generator_service(self) -> AiTestGeneratorService:
        """Get or create the AI test generator service."""
        if self._ai_test_generator_service is None:
            self._ai_test_generator_service = AiTestGeneratorService(
                event_manager=self.event_manager,
            )
        return self._ai_test_generator_service  # type: ignore[no-any-return]

    def _handle_generate_test_with_ai(self, params: dict[str, Any]) -> dict[str, Any]:
        """
        Handle AI test generation command.

        Uses the configured AI provider to generate test code from a natural
        language description and optional page analysis context.

        Args:
            params: Dict with:
                - user_prompt: str - User's description of the test
                - test_type: str - Type of test (playwright_cdp, qontinui_vision, etc.)
                - page_analysis: dict | None - Optional page analysis data
                - multi_request_analysis: dict | None - Optional multi-request ground truth
                - collected_analyses: dict | None - Optional collected analyses (multiple types)
                - ai_provider: str - AI provider (claude_cli, claude_api, gemini_cli, gemini_api)
                - ai_settings: dict | None - Provider-specific settings

        Returns:
            Dict with success, code, and error fields.
        """
        import sys

        user_prompt = params.get("user_prompt", "")
        test_type = params.get("test_type", "playwright_cdp")
        page_analysis = params.get("page_analysis")
        multi_request_analysis = params.get("multi_request_analysis")
        collected_analyses = params.get("collected_analyses")
        reference_documents = params.get("reference_documents")
        workflow_run_context = params.get("workflow_run_context")
        ai_provider = params.get("ai_provider", "claude_cli")
        ai_settings = params.get("ai_settings", {})

        print(
            f"[info    ] EXECUTOR: _handle_generate_test_with_ai called with provider={ai_provider}, test_type={test_type}",
            file=sys.stderr,
            flush=True,
        )

        if not user_prompt:
            return {
                "success": False,
                "error": "user_prompt is required",
            }

        try:
            service = self._get_ai_test_generator_service()

            result = service.generate_test(
                user_prompt=user_prompt,
                test_type=test_type,
                page_analysis=page_analysis,
                multi_request_analysis=multi_request_analysis,
                collected_analyses=collected_analyses,
                reference_documents=reference_documents,
                workflow_run_context=workflow_run_context,
                ai_provider=ai_provider,
                ai_settings=ai_settings,
            )

            return result

        except Exception as e:
            import traceback

            return {
                "success": False,
                "error": f"AI test generation failed: {e}",
                "traceback": traceback.format_exc(),
            }

    def _get_ai_shell_command_generator_service(self) -> AiShellCommandGeneratorService:
        """Get or create the AI shell command generator service."""
        if self._ai_shell_command_generator_service is None:
            self._ai_shell_command_generator_service = AiShellCommandGeneratorService(
                event_manager=self.event_manager,
            )
        return self._ai_shell_command_generator_service  # type: ignore[no-any-return]

    def _handle_generate_shell_command_with_ai(
        self, params: dict[str, Any]
    ) -> dict[str, Any]:
        """
        Handle AI shell command generation.

        Uses the configured AI provider to generate shell commands from a natural
        language description.

        Args:
            params: Dict with:
                - user_prompt: str - User's description of the command
                - target_os: str - Target OS (windows, linux, macos)
                - category: str | None - Command category (git, npm, docker, etc.)
                - ai_provider: str - AI provider (claude_cli, claude_api, gemini_cli, gemini_api)
                - ai_settings: dict | None - Provider-specific settings

        Returns:
            Dict with success, command, description, and error fields.
        """
        import datetime
        import os
        import sys

        # Debug log file
        debug_log = os.path.join(
            os.environ.get("USERPROFILE", "C:\\Users\\Joshua"),
            ".qontinui",
            "ai-shell-debug.log",
        )
        os.makedirs(os.path.dirname(debug_log), exist_ok=True)

        def debug(msg):
            ts = datetime.datetime.now().isoformat()
            line = f"[{ts}] {msg}\n"
            with open(debug_log, "a", encoding="utf-8") as f:
                f.write(line)
            print(f"[DEBUG] {msg}", file=sys.stderr, flush=True)

        debug("=" * 60)
        debug("_handle_generate_shell_command_with_ai CALLED")
        debug(f"params keys: {list(params.keys())}")

        user_prompt = params.get("user_prompt", "")
        target_os = params.get("target_os", "windows")
        category = params.get("category")
        ai_provider = params.get("ai_provider", "claude_cli")
        ai_settings = params.get("ai_settings", {})

        debug(f"user_prompt (first 100 chars): {user_prompt[:100]!r}")
        debug(f"target_os: {target_os}")
        debug(f"category: {category}")
        debug(f"ai_provider: {ai_provider}")
        debug(f"ai_settings: {ai_settings}")

        if not user_prompt:
            debug("ERROR: user_prompt is empty")
            return {
                "success": False,
                "error": "user_prompt is required",
            }

        try:
            debug("Getting AI shell command generator service...")
            service = self._get_ai_shell_command_generator_service()
            debug(f"Service obtained: {type(service)}")

            debug("Calling service.generate_command()...")
            result = service.generate_command(
                user_prompt=user_prompt,
                target_os=target_os,
                category=category,
                ai_provider=ai_provider,
                ai_settings=ai_settings,
            )
            debug(
                f"service.generate_command() returned: success={result.get('success')}, error={result.get('error')!r}"
            )
            debug(
                f"command (first 200 chars): {str(result.get('command', ''))[:200]!r}"
            )

            return result

        except Exception as e:
            import traceback

            tb = traceback.format_exc()
            debug(f"EXCEPTION: {e}")
            debug(f"TRACEBACK:\n{tb}")

            return {
                "success": False,
                "error": f"AI shell command generation failed: {e}",
                "traceback": tb,
            }

    def _get_ai_builder_generator_service(self) -> AiBuilderGeneratorService:
        """Get or create the AI builder generator service."""
        if self._ai_builder_generator_service is None:
            self._ai_builder_generator_service = AiBuilderGeneratorService(
                event_manager=self.event_manager,
            )
        return self._ai_builder_generator_service  # type: ignore[no-any-return]

    def _handle_generate_context_with_ai(
        self, params: dict[str, Any]
    ) -> dict[str, Any]:
        """Handle AI context generation for knowledge base entries."""
        user_prompt = params.get("user_prompt", "")
        ai_provider = params.get("ai_provider", "claude_cli")
        ai_settings = params.get("ai_settings", {})

        if not user_prompt:
            return {"success": False, "error": "user_prompt is required"}

        try:
            service = self._get_ai_builder_generator_service()
            return service.generate_context(
                user_prompt=user_prompt,
                ai_provider=ai_provider,
                ai_settings=ai_settings,
            )
        except Exception as e:
            import traceback

            return {
                "success": False,
                "error": f"Context generation failed: {e}",
                "traceback": traceback.format_exc(),
            }

    def _handle_generate_api_request_with_ai(
        self, params: dict[str, Any]
    ) -> dict[str, Any]:
        """Handle AI API request template generation."""
        user_prompt = params.get("user_prompt", "")
        base_url = params.get("base_url")
        ai_provider = params.get("ai_provider", "claude_cli")
        ai_settings = params.get("ai_settings", {})

        if not user_prompt:
            return {"success": False, "error": "user_prompt is required"}

        try:
            service = self._get_ai_builder_generator_service()
            return service.generate_api_request(
                user_prompt=user_prompt,
                base_url=base_url,
                ai_provider=ai_provider,
                ai_settings=ai_settings,
            )
        except Exception as e:
            import traceback

            return {
                "success": False,
                "error": f"API request generation failed: {e}",
                "traceback": traceback.format_exc(),
            }

    def _handle_generate_task_prompt_with_ai(
        self, params: dict[str, Any]
    ) -> dict[str, Any]:
        """Handle AI task prompt generation/improvement."""
        user_prompt = params.get("user_prompt", "")
        mode = params.get("mode", "generate")
        ai_provider = params.get("ai_provider", "claude_cli")
        ai_settings = params.get("ai_settings", {})

        if not user_prompt:
            return {"success": False, "error": "user_prompt is required"}

        try:
            service = self._get_ai_builder_generator_service()
            return service.generate_task_prompt(
                user_prompt=user_prompt,
                mode=mode,
                ai_provider=ai_provider,
                ai_settings=ai_settings,
            )
        except Exception as e:
            import traceback

            return {
                "success": False,
                "error": f"Task prompt generation failed: {e}",
                "traceback": traceback.format_exc(),
            }

    def _handle_suggest_exploration_strategy_with_ai(
        self, params: dict[str, Any]
    ) -> dict[str, Any]:
        """Handle AI exploration strategy suggestion."""
        user_goal = params.get("user_goal", "")
        available_states = params.get("available_states", [])
        available_transitions = params.get("available_transitions", [])
        ai_provider = params.get("ai_provider", "claude_cli")
        ai_settings = params.get("ai_settings", {})

        if not user_goal:
            return {"success": False, "error": "user_goal is required"}

        try:
            service = self._get_ai_builder_generator_service()
            return service.suggest_exploration_strategy(
                user_goal=user_goal,
                available_states=available_states,
                available_transitions=available_transitions,
                ai_provider=ai_provider,
                ai_settings=ai_settings,
            )
        except Exception as e:
            import traceback

            return {
                "success": False,
                "error": f"Exploration strategy suggestion failed: {e}",
                "traceback": traceback.format_exc(),
            }

    def _handle_generate_test_and_agentic_step(
        self, params: dict[str, Any]
    ) -> dict[str, Any]:
        """Handle AI test and agentic step generation."""
        user_prompt = params.get("user_prompt", "")
        page_context = params.get("page_context")
        context_ids = params.get("context_ids")  # Context IDs from context library
        ai_provider = params.get("ai_provider", "claude_cli")
        ai_settings = params.get("ai_settings", {})

        if not user_prompt:
            return {"success": False, "error": "user_prompt is required"}

        # Fetch contexts if context_ids provided
        contexts_content = None
        if context_ids and len(context_ids) > 0:
            try:
                contexts_content = self._fetch_contexts_content(context_ids)
                self.event_manager.emit_log(
                    "info",
                    f"[GENERATE_TEST] Fetched {len(context_ids)} contexts for AI prompt",
                )
            except Exception as e:
                self.event_manager.emit_log(
                    "warning",
                    f"[GENERATE_TEST] Failed to fetch contexts: {e}",
                )

        try:
            service = self._get_ai_builder_generator_service()
            return service.generate_test_and_agentic_step(
                user_prompt=user_prompt,
                page_context=page_context,
                contexts_content=contexts_content,
                ai_provider=ai_provider,
                ai_settings=ai_settings,
            )
        except Exception as e:
            import traceback

            return {
                "success": False,
                "error": f"Test and agentic step generation failed: {e}",
                "traceback": traceback.format_exc(),
            }

    def _handle_explore_flow_step(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle AI-driven flow exploration step.

        The AI analyzes current page elements and user's goal to determine
        what action to take next (click, type, wait, or done).
        """
        user_prompt = params.get("user_prompt", "")
        current_elements = params.get("current_elements", [])
        current_url = params.get("current_url", "")
        current_title = params.get("current_title", "")
        captured_pages = params.get("captured_pages", [])
        step_number = params.get("step_number", 1)
        ai_provider = params.get("ai_provider", "claude_cli")
        ai_settings = params.get("ai_settings", {})

        if not user_prompt:
            return {"success": False, "error": "user_prompt is required"}

        if not current_elements:
            return {"success": False, "error": "current_elements is required"}

        try:
            service = self._get_ai_builder_generator_service()
            return service.explore_flow_step(
                user_prompt=user_prompt,
                current_elements=current_elements,
                current_url=current_url,
                current_title=current_title,
                captured_pages=captured_pages,
                step_number=step_number,
                ai_provider=ai_provider,
                ai_settings=ai_settings,
            )
        except Exception as e:
            import traceback

            return {
                "success": False,
                "error": f"Flow exploration step failed: {e}",
                "traceback": traceback.format_exc(),
            }

    def _fetch_contexts_content(self, context_ids: list[str]) -> str | None:
        """Fetch context content from the runner HTTP API."""
        import requests

        contexts_parts = []
        for ctx_id in context_ids:
            try:
                response = requests.get(
                    f"{self._get_runner_api_base()}/contexts/{ctx_id}",
                    timeout=5,
                )
                if response.status_code == 200:
                    data = response.json()
                    if data.get("success") and data.get("data"):
                        ctx = data["data"]
                        name = ctx.get("name", "Unknown")
                        category = ctx.get("category", "")
                        content = ctx.get("content", "")
                        if content:
                            # Format as XML block (same as context injection elsewhere)
                            ctx_block = f'<context name="{name}"'
                            if category:
                                ctx_block += f' category="{category}"'
                            ctx_block += f">\n{content}\n</context>"
                            contexts_parts.append(ctx_block)
            except Exception as e:
                self.event_manager.emit_log(
                    "warning",
                    f"[FETCH_CONTEXT] Failed to fetch context {ctx_id}: {e}",
                )

        if contexts_parts:
            return "## Relevant Context\n\n" + "\n\n".join(contexts_parts)
        return None

    def _handle_run_vision_extraction(self, params: dict[str, Any]) -> dict[str, Any]:
        """
        Handle run vision extraction command.

        Runs SAM3, Edge Detection, and/or OCR on a screenshot.

        Args:
            params: Extraction configuration with:
                - screenshot: Base64-encoded image OR file path
                - techniques: List of techniques ["edge", "sam3", "ocr"]
                - Edge detection config: canny_low, canny_high, min_contour_area
                - SAM3 config: points_per_side, pred_iou_thresh, stability_score_thresh
                - OCR config: ocr_engine, ocr_languages, ocr_confidence_threshold
                - Fusion config: iou_threshold

        Returns:
            Dict with extraction results.
        """
        import sys

        print(
            f"[info    ] EXECUTOR: _handle_run_vision_extraction called with {len(params.get('screenshot', ''))} char screenshot",
            file=sys.stderr,
            flush=True,
        )

        try:
            service = self._get_vision_extraction_service()

            # Run extraction (synchronous since VisionExtractionService.extract is sync)
            config = params.get("config", params)  # Support both nested and flat config
            result = service.extract(config)

            if result.get("success"):
                self.event_manager.emit_event(
                    EventType.EXTRACTION_STARTED,
                    {
                        "extraction_id": result.get("extraction_id"),
                        "technique": "vision",
                        "techniques_run": result.get("techniques_run", []),
                    },
                )

            print(
                f"[info    ] EXECUTOR: vision_extraction result: success={result.get('success')}, "
                f"edge={len(result.get('edge_results', []))}, "
                f"sam3={len(result.get('sam3_results', []))}, "
                f"ocr={len(result.get('ocr_results', []))}",
                file=sys.stderr,
                flush=True,
            )
            return result

        except Exception as e:
            print(
                f"[error   ] EXECUTOR: Failed to run vision extraction: {e}",
                file=sys.stderr,
                flush=True,
            )
            print(
                f"[error   ] EXECUTOR: Traceback: {traceback.format_exc()}",
                file=sys.stderr,
                flush=True,
            )
            self.event_manager.emit_log(
                "error", f"Failed to run vision extraction: {e}"
            )
            return {"success": False, "error": str(e)}

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
            self._async_thread = threading.Thread(
                target=self._start_async_loop, daemon=True
            )
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

            future = asyncio.run_coroutine_threadsafe(
                service.start_extraction(config), loop
            )
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
                f"[error   ] EXECUTOR: Failed to start extraction: {e}",
                file=sys.stderr,
                flush=True,
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
            self.event_manager.emit_log(
                "error", f"Failed to get extraction status: {e}"
            )
            return {"success": False, "error": str(e)}

    def _get_playwright_collector_service(self) -> PlaywrightCollectorService:
        """Get or create the Playwright collector service."""
        if self._playwright_collector_service is None:
            self._playwright_collector_service = PlaywrightCollectorService(
                event_manager=self.event_manager,
            )
        return self._playwright_collector_service  # type: ignore[no-any-return]

    def _handle_start_playwright_collection(
        self, params: dict[str, Any]
    ) -> dict[str, Any]:
        """Handle start Playwright collection command."""
        import sys

        print(
            f"[info    ] EXECUTOR: _handle_start_playwright_collection called with params: {params}",
            file=sys.stderr,
            flush=True,
        )

        try:
            service = self._get_playwright_collector_service()
            result = service.start_collection(params)

            print(
                f"[info    ] EXECUTOR: start_playwright_collection result: {result}",
                file=sys.stderr,
                flush=True,
            )
            return result

        except Exception as e:
            self.event_manager.emit_log(
                "error", f"Failed to start Playwright collection: {e}"
            )
            import traceback

            traceback.print_exc(file=sys.stderr)
            return {"success": False, "error": str(e)}

    def _handle_get_playwright_collection_status(
        self, params: dict[str, Any]
    ) -> dict[str, Any]:
        """Handle get Playwright collection status command."""
        try:
            if self._playwright_collector_service is None:
                return {
                    "success": True,
                    "status": "idle",
                    "job_id": None,
                }

            job_id = params.get("job_id")
            result: dict[str, Any] = self._playwright_collector_service.get_job_status(
                job_id
            )
            return result

        except Exception as e:
            self.event_manager.emit_log(
                "error", f"Failed to get Playwright collection status: {e}"
            )
            return {"success": False, "error": str(e)}

    def _handle_get_playwright_collection_results(
        self, params: dict[str, Any]
    ) -> dict[str, Any]:
        """Handle get Playwright collection results command."""
        try:
            if self._playwright_collector_service is None:
                return {"success": False, "error": "No Playwright collection service"}

            job_id = params.get("job_id")
            result: dict[str, Any] = self._playwright_collector_service.get_results(
                job_id
            )
            return result

        except Exception as e:
            self.event_manager.emit_log(
                "error", f"Failed to get Playwright collection results: {e}"
            )
            return {"success": False, "error": str(e)}

    def _handle_stop_playwright_collection(self) -> dict[str, Any]:
        """Handle stop Playwright collection command."""
        try:
            if self._playwright_collector_service is None:
                return {"success": False, "error": "No collection in progress"}

            result: dict[str, Any] = (
                self._playwright_collector_service.stop_collection()
            )
            return result

        except Exception as e:
            self.event_manager.emit_log(
                "error", f"Failed to stop Playwright collection: {e}"
            )
            return {"success": False, "error": str(e)}

    # =========================================================================
    # UI Bridge Exploration Handlers
    # =========================================================================

    def _get_ui_bridge_explorer_service(self) -> UIBridgeExplorerService:
        """Get or create the UI Bridge explorer service."""
        if self._ui_bridge_explorer_service is None:
            self._ui_bridge_explorer_service = UIBridgeExplorerService(
                event_manager=self.event_manager,
            )
        return self._ui_bridge_explorer_service

    def _handle_start_ui_bridge_exploration(
        self, params: dict[str, Any]
    ) -> dict[str, Any]:
        """Handle start UI Bridge exploration command.

        Starts an exploration job in the background and returns a job ID.
        Use get_ui_bridge_exploration_status and get_ui_bridge_exploration_results
        to poll for progress and retrieve results.
        """
        import sys

        print(
            f"[info    ] EXECUTOR: _handle_start_ui_bridge_exploration called with params: {params}",
            file=sys.stderr,
            flush=True,
        )

        try:
            service = self._get_ui_bridge_explorer_service()
            result = service.start_exploration(params)

            if result.get("success"):
                self.event_manager.emit_log(
                    "info",
                    f"[UI Bridge] Started exploration job: {result.get('job_id')}",
                )

            return result

        except Exception as e:
            import traceback

            self.event_manager.emit_log(
                "error", f"[UI Bridge] Start exploration failed: {e}"
            )
            traceback.print_exc(file=sys.stderr)
            return {"success": False, "error": str(e)}

    def _handle_get_ui_bridge_exploration_status(
        self, params: dict[str, Any]
    ) -> dict[str, Any]:
        """Handle get UI Bridge exploration status command.

        Returns the current status of an exploration job.
        """
        import sys

        print(
            f"[info    ] EXECUTOR: _handle_get_ui_bridge_exploration_status called with params: {params}",
            file=sys.stderr,
            flush=True,
        )

        try:
            service = self._get_ui_bridge_explorer_service()
            job_id = params.get("job_id")
            return service.get_job_status(job_id)

        except Exception as e:
            import traceback

            self.event_manager.emit_log(
                "error", f"[UI Bridge] Get exploration status failed: {e}"
            )
            traceback.print_exc(file=sys.stderr)
            return {"success": False, "error": str(e)}

    def _handle_get_ui_bridge_exploration_results(
        self, params: dict[str, Any]
    ) -> dict[str, Any]:
        """Handle get UI Bridge exploration results command.

        Returns the results of a completed exploration job.
        """
        import sys

        print(
            f"[info    ] EXECUTOR: _handle_get_ui_bridge_exploration_results called with params: {params}",
            file=sys.stderr,
            flush=True,
        )

        try:
            service = self._get_ui_bridge_explorer_service()
            job_id = params.get("job_id")
            return service.get_results(job_id)

        except Exception as e:
            import traceback

            self.event_manager.emit_log(
                "error", f"[UI Bridge] Get exploration results failed: {e}"
            )
            traceback.print_exc(file=sys.stderr)
            return {"success": False, "error": str(e)}

    def _handle_stop_ui_bridge_exploration(
        self, params: dict[str, Any]
    ) -> dict[str, Any]:
        """Handle stop UI Bridge exploration command.

        Stops the current exploration job.
        """
        import sys

        print(
            "[info    ] EXECUTOR: _handle_stop_ui_bridge_exploration called",
            file=sys.stderr,
            flush=True,
        )

        try:
            service = self._get_ui_bridge_explorer_service()
            return service.stop_exploration()

        except Exception as e:
            import traceback

            self.event_manager.emit_log(
                "error", f"[UI Bridge] Stop exploration failed: {e}"
            )
            traceback.print_exc(file=sys.stderr)
            return {"success": False, "error": str(e)}

    def _handle_discover_states_from_renders(
        self, params: dict[str, Any]
    ) -> dict[str, Any]:
        """Handle discover states from render logs command.

        Runs co-occurrence analysis on existing render logs to discover states.
        This is separate from exploration - it only analyzes provided render data.

        Args:
            params: Command parameters:
                - render_logs: Array of DOM snapshot render log entries

        Returns:
            Dictionary with:
                - success: Whether discovery succeeded
                - data: UIBridgeStateDiscoveryResult with states, elements, etc.
                - error: Error message (if failed)
        """
        import sys

        print(
            "[info    ] EXECUTOR: _handle_discover_states_from_renders called",
            file=sys.stderr,
            flush=True,
        )

        try:
            if not QONTINUI_AVAILABLE:
                return {
                    "success": False,
                    "error": "Qontinui library not available",
                }

            # Get render logs from params
            render_logs = params.get("render_logs", [])

            if not render_logs:
                return {
                    "success": False,
                    "error": "No render_logs provided",
                }

            self.event_manager.emit_log(
                "info",
                f"[UI Bridge] Starting state discovery on {len(render_logs)} render logs",
            )

            # Import and call the qontinui library function
            from qontinui.discovery.ui_bridge_adapter import (
                discover_states_from_renders,
            )

            result = discover_states_from_renders(render_logs)

            self.event_manager.emit_log(
                "info",
                f"[UI Bridge] State discovery complete: {len(result.states)} states, "
                f"{result.unique_element_count} unique elements from {result.render_count} renders",
            )

            return {
                "success": True,
                "data": result.to_dict(),
            }

        except Exception as e:
            import traceback

            self.event_manager.emit_log(
                "error", f"[UI Bridge] Discover states from renders failed: {e}"
            )
            traceback.print_exc(file=sys.stderr)
            return {"success": False, "error": str(e)}

    def _handle_discover_states_from_fingerprints(
        self, params: dict[str, Any]
    ) -> dict[str, Any]:
        """Handle discover states from fingerprint co-occurrence data.

        Uses the FingerprintStateDiscovery class to analyze element fingerprints
        collected across multiple page captures and discover states based on
        which elements consistently appear together.

        Args:
            params: Command parameters:
                - cooccurrence_export: CooccurrenceExport data from UI Bridge capture session
                    - sessionId: Unique session identifier
                    - presenceMatrix: Array of capture snapshots with fingerprint data
                    - transitions: Array of action transitions between captures
                    - fingerprintStats: Statistics for each fingerprint hash
                    - stateCandidates: Pre-computed state candidates (optional)
                - config: Optional discovery configuration
                    - minCooccurrenceRate: Minimum co-occurrence rate (default: 0.95)

        Returns:
            Dictionary with:
                - success: Whether discovery succeeded
                - data: Discovered states and transitions
                    - states: Array of DiscoveredFingerprintState objects
                    - transitions: Array of state transitions
                    - statistics: Discovery statistics
                - error: Error message (if failed)
        """
        import sys

        print(
            "[info    ] EXECUTOR: _handle_discover_states_from_fingerprints called",
            file=sys.stderr,
            flush=True,
        )

        try:
            if not QONTINUI_AVAILABLE:
                return {
                    "success": False,
                    "error": "Qontinui library not available",
                }

            # Get co-occurrence export from params (as raw dict)
            cooccurrence_export = params.get("cooccurrence_export")

            if not cooccurrence_export:
                return {
                    "success": False,
                    "error": "No cooccurrence_export provided",
                }

            # Get optional config
            user_config = params.get("config", {})
            min_cooccurrence_rate = user_config.get("minCooccurrenceRate", 0.95)

            self.event_manager.emit_log(
                "info",
                f"[UI Bridge] Starting fingerprint state discovery "
                f"(min_cooccurrence_rate={min_cooccurrence_rate})",
            )

            # Import the FingerprintStateDiscovery class
            from qontinui.state_machine.fingerprint_state_discovery import (
                FingerprintStateDiscovery,
                FingerprintStateDiscoveryConfig,
            )

            # Log input data summary
            presence_matrix = cooccurrence_export.get("presenceMatrix", [])
            transitions_data = cooccurrence_export.get("transitions", [])
            fingerprint_stats = cooccurrence_export.get("fingerprintStats", {})

            self.event_manager.emit_log(
                "info",
                f"[UI Bridge] Input data: "
                f"{len(presence_matrix)} captures, "
                f"{len(transitions_data)} transitions, "
                f"{len(fingerprint_stats)} unique fingerprints",
            )

            # Create discovery config
            config = FingerprintStateDiscoveryConfig(
                min_cooccurrence_rate=min_cooccurrence_rate,
            )

            # Create discovery instance
            discovery = FingerprintStateDiscovery(config=config)

            # Load data from raw dict (the method handles parsing internally)
            discovery.load_cooccurrence_export(cooccurrence_export)

            # Run discovery
            discovered_states = discovery.discover_states()

            self.event_manager.emit_log(
                "info",
                f"[UI Bridge] Fingerprint state discovery complete: "
                f"{len(discovered_states)} states discovered",
            )

            # Get raw transitions
            raw_transitions = discovery.get_transitions()

            # Map raw transitions to state transitions
            # We need to find which state each transition's fingerprints belong to
            state_transitions = self._map_transitions_to_states(
                raw_transitions, discovered_states
            )

            # Get statistics
            stats = discovery.get_statistics()

            # Build result
            result = {
                "states": [
                    {
                        "stateId": state.state_id,
                        "name": state.name,
                        "fingerprintHashes": list(state.fingerprint_hashes),
                        "elementIds": list(state.element_ids),
                        "positionZone": state.position_zone,
                        "landmarkContext": state.landmark_context,
                        "isGlobal": state.is_global,
                        "isModal": state.is_modal,
                        "repeatPatternCount": state.repeat_pattern_count,
                        "confidence": state.confidence,
                        "observationCount": state.observation_count,
                    }
                    for state in discovered_states
                ],
                "transitions": state_transitions,
                "statistics": {
                    "totalCaptures": stats.get("total_captures", 0),
                    "totalTransitions": stats.get("total_transitions", 0),
                    "uniqueFingerprints": stats.get("total_fingerprints", 0),
                    "discoveredStates": stats.get("discovered_states", 0),
                    "globalStates": stats.get("global_states", 0),
                    "modalStates": stats.get("modal_states", 0),
                    "discoveredTransitions": len(state_transitions),
                },
            }

            return {
                "success": True,
                "data": result,
            }

        except Exception as e:
            import traceback

            self.event_manager.emit_log(
                "error", f"[UI Bridge] Discover states from fingerprints failed: {e}"
            )
            traceback.print_exc(file=sys.stderr)
            return {"success": False, "error": str(e)}

    def _map_transitions_to_states(
        self,
        raw_transitions: list[Any],
        discovered_states: list[Any],
    ) -> list[dict[str, Any]]:
        """Map raw transitions to state transitions.

        Args:
            raw_transitions: List of TransitionRecord objects
            discovered_states: List of DiscoveredFingerprintState objects

        Returns:
            List of state transition dictionaries
        """
        # Build a mapping from fingerprint hash to state
        fp_to_state: dict[str, str] = {}
        for state in discovered_states:
            for fp_hash in state.fingerprint_hashes:
                fp_to_state[fp_hash] = state.state_id

        # Track unique state transitions
        transition_counts: dict[tuple[str, str, str], int] = {}

        for t in raw_transitions:
            # Find which states the appeared/disappeared fingerprints belong to
            from_states: set[str] = set()
            to_states: set[str] = set()

            for fp_hash in t.disappeared_fingerprints:
                if fp_hash in fp_to_state:
                    from_states.add(fp_to_state[fp_hash])

            for fp_hash in t.appeared_fingerprints:
                if fp_hash in fp_to_state:
                    to_states.add(fp_to_state[fp_hash])

            # Create transitions for each from/to state pair
            for from_state in from_states:
                for to_state in to_states:
                    if from_state != to_state:
                        key = (from_state, to_state, t.action_type)
                        transition_counts[key] = transition_counts.get(key, 0) + 1

        # Convert to result format
        result = []
        for (from_state, to_state, action_type), count in transition_counts.items():
            result.append(
                {
                    "fromStateId": from_state,
                    "toStateId": to_state,
                    "actionType": action_type,
                    "count": count,
                }
            )

        return result

    def _handle_run_ui_bridge_exploration(
        self, params: dict[str, Any]
    ) -> dict[str, Any]:
        """Handle automatic UI Bridge exploration.

        Uses the UIBridgeExplorer to systematically explore interactive elements
        on a web page via the browser extension, building co-occurrence data
        for fingerprint-based state discovery.

        Args:
            params: Command parameters:
                - runner_url: URL of the qontinui-runner (default: http://127.0.0.1:{QONTINUI_PORT})
                - config: Optional exploration configuration
                    - max_depth: Maximum navigation depth (default: 2)
                    - max_elements_per_page: Max elements per page (default: 20)
                    - max_total_elements: Max total elements (default: 100)
                    - action_delay_ms: Delay between actions (default: 500)
                    - blocked_keywords: Keywords to skip
                    - safe_keywords: Safe keywords
                    - capture_screenshots: Whether to capture screenshots

        Returns:
            Dictionary with:
                - success: Whether exploration succeeded
                - data: Exploration results
                    - exploration_id: Unique ID
                    - elements_discovered: Count of discovered elements
                    - elements_explored: Count of explored elements
                    - errors: List of errors
                    - state_discovery_result: Discovered states
                    - cooccurrence_export: Raw export data
                - error: Error message (if failed)
        """
        import asyncio
        import sys

        print(
            "[info    ] EXECUTOR: _handle_run_ui_bridge_exploration called",
            file=sys.stderr,
            flush=True,
        )

        try:
            if not QONTINUI_AVAILABLE:
                return {
                    "success": False,
                    "error": "Qontinui library not available",
                }

            # Get parameters
            runner_url = params.get("runner_url", self._get_runner_api_base())
            user_config = params.get("config", {})

            self.event_manager.emit_log(
                "info",
                f"[UI Bridge] Starting automatic exploration (runner_url={runner_url})",
            )

            # Import explorer
            from qontinui.discovery.target_connection import ExplorationConfig
            from qontinui.discovery.ui_bridge_explorer import (
                UIBridgeExplorer,
            )

            # Build exploration config
            config = ExplorationConfig(
                target_type="extension",  # Use extension connection
                connection_url=runner_url,
                max_depth=user_config.get("max_depth", 2),
                max_elements_per_page=user_config.get("max_elements_per_page", 20),
                max_total_elements=user_config.get("max_total_elements", 100),
                action_delay_ms=user_config.get("action_delay_ms", 500),
                blocked_keywords=user_config.get("blocked_keywords", []),
                safe_keywords=user_config.get("safe_keywords", []),
                capture_screenshots=user_config.get("capture_screenshots", False),
                record_render_logs=True,
            )

            # Progress callback to emit events
            def on_progress(
                message: str,
                elements_discovered: int,
                elements_explored: int,
                current_element: str | None,
            ) -> bool:
                self.event_manager.emit_log(
                    "info",
                    f"[Exploration] {message} "
                    f"(discovered={elements_discovered}, explored={elements_explored})",
                )
                return True  # Continue exploration

            # Run exploration in event loop
            async def run_exploration():
                async with UIBridgeExplorer(
                    config, on_progress=on_progress
                ) as explorer:
                    return await explorer.explore()

            # Get or create event loop
            try:
                loop = asyncio.get_event_loop()
            except RuntimeError:
                loop = asyncio.new_event_loop()
                asyncio.set_event_loop(loop)

            result = loop.run_until_complete(run_exploration())

            self.event_manager.emit_log(
                "info",
                f"[UI Bridge] Exploration complete: "
                f"{result.elements_discovered} discovered, "
                f"{result.elements_explored} explored, "
                f"{len(result.errors)} errors",
            )

            # Build response
            response_data: dict[str, Any] = {
                "exploration_id": result.exploration_id,
                "elements_discovered": result.elements_discovered,
                "elements_explored": result.elements_explored,
                "errors": result.errors,
            }

            # Include state discovery result if available
            if result.state_discovery_result:
                response_data["state_discovery_result"] = (
                    result.state_discovery_result.to_dict()
                )

            # Include cooccurrence export if available
            if result.cooccurrence_export:
                response_data["cooccurrence_export"] = result.cooccurrence_export

            return {
                "success": True,
                "data": response_data,
            }

        except Exception as e:
            import traceback

            self.event_manager.emit_log("error", f"[UI Bridge] Exploration failed: {e}")
            traceback.print_exc(file=sys.stderr)
            return {"success": False, "error": str(e)}

    # =========================================================================
    # UI-TARS Extraction Handlers
    # =========================================================================

    def _get_uitars_extraction_service(self) -> UITarsExtractionService:
        """Get or create the UI-TARS extraction service."""
        if self._uitars_extraction_service is None:
            self._uitars_extraction_service = get_uitars_extraction_service(
                emit_log_fn=self.event_manager.emit_log,
                emit_event_fn=self._emit_event_wrapper,
            )
        return self._uitars_extraction_service

    def _handle_start_uitars_extraction(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle start UI-TARS extraction command."""
        import sys

        print(
            f"[info    ] EXECUTOR: _handle_start_uitars_extraction called with params: {params}",
            file=sys.stderr,
            flush=True,
        )

        try:
            service = self._get_uitars_extraction_service()

            # Check if UI-TARS is available
            if not service.is_available():
                self.event_manager.emit_log(
                    "warning",
                    "[UI-TARS] UI-TARS not available, will run simulated extraction",
                )

            # Start extraction
            config = params.get("config", params)
            result = service.start_extraction(config)

            print(
                f"[info    ] EXECUTOR: start_uitars_extraction result: {result}",
                file=sys.stderr,
                flush=True,
            )
            return result

        except Exception as e:
            self.event_manager.emit_log(
                "error", f"Failed to start UI-TARS extraction: {e}"
            )
            import traceback

            traceback.print_exc(file=sys.stderr)
            return {"success": False, "error": str(e)}

    def _handle_stop_uitars_extraction(self) -> dict[str, Any]:
        """Handle stop UI-TARS extraction command."""
        try:
            if self._uitars_extraction_service is None:
                return {"success": False, "error": "No UI-TARS extraction in progress"}

            return self._uitars_extraction_service.stop_extraction()

        except Exception as e:
            self.event_manager.emit_log(
                "error", f"Failed to stop UI-TARS extraction: {e}"
            )
            return {"success": False, "error": str(e)}

    def _handle_get_uitars_extraction_status(self) -> dict[str, Any]:
        """Handle get UI-TARS extraction status command."""
        try:
            if self._uitars_extraction_service is None:
                return {
                    "success": True,
                    "status": "idle",
                    "current_step": 0,
                    "max_steps": 0,
                    "elapsed_seconds": 0,
                    "states_discovered": 0,
                    "transitions_discovered": 0,
                    "uitars_available": False,
                }

            status = self._uitars_extraction_service.get_status()
            return {"success": True, **status}

        except Exception as e:
            self.event_manager.emit_log(
                "error", f"Failed to get UI-TARS extraction status: {e}"
            )
            return {"success": False, "error": str(e)}

    def _handle_get_uitars_extraction_results(self) -> dict[str, Any]:
        """Handle get UI-TARS extraction results command."""
        try:
            if self._uitars_extraction_service is None:
                return {
                    "success": True,
                    "states": [],
                    "transitions": [],
                    "total_steps": 0,
                    "total_screenshots": 0,
                    "exploration_time_seconds": 0,
                }

            results = self._uitars_extraction_service.get_results()
            return {"success": True, **results}

        except Exception as e:
            self.event_manager.emit_log(
                "error", f"Failed to get UI-TARS extraction results: {e}"
            )
            return {"success": False, "error": str(e)}

    def _handle_execute_workflow(self, params: dict[str, Any]) -> dict[str, Any]:
        """
        Handle execute_workflow command from web app.

        Receives a full unified workflow definition from the web app and
        forwards it to the local Rust MCP API's execute-inline endpoint.
        This approach works for remote runners that don't have the workflow
        in their local SQLite DB.

        The execute-inline endpoint blocks until execution finishes, so we
        run it in a background thread and return immediately.

        Args:
            params: Dictionary containing:
                - execution_id: Unique ID for tracking this execution
                - workflow: Full unified workflow configuration
                - variables: Optional variables to pass to workflow

        Returns:
            Dictionary with success status and execution details
        """
        import sys
        import threading

        execution_id = params.get("execution_id")
        workflow = params.get("workflow")

        print(
            f"[info    ] EXECUTOR: _handle_execute_workflow called with execution_id={execution_id}",
            file=sys.stderr,
            flush=True,
        )

        if not workflow:
            return {"success": False, "error": "No workflow configuration provided"}

        if not execution_id:
            return {"success": False, "error": "No execution_id provided"}

        def _run_workflow_background():
            import requests

            try:
                self.event_manager.emit_log(
                    "info",
                    f"Starting remote workflow execution via execute-inline: {workflow.get('name', 'Unknown')}",
                )

                # Notify via WebSocket that execution has started
                if self.websocket_handler and self.websocket_handler.is_connected:
                    execution_event = {
                        "type": "execution_started",
                        "data": {
                            "execution_id": execution_id,
                            "workflow_name": workflow.get("name", "Unknown"),
                        },
                    }
                    self.websocket_handler.send_message(json.dumps(execution_event))

                # Forward to the local Rust MCP API's execute-inline endpoint
                resp = requests.post(
                    f"{self._get_runner_api_base()}/unified-workflows/execute-inline",
                    json={
                        "name": workflow.get("name", "Remote Workflow"),
                        "description": workflow.get("description", ""),
                        "setup_steps": workflow.get("setup_steps", []),
                        "verification_steps": workflow.get("verification_steps", []),
                        "agentic_steps": workflow.get("agentic_steps", []),
                        "completion_steps": workflow.get("completion_steps", []),
                        "max_iterations": workflow.get("max_iterations", 10),
                        "timeout_seconds": workflow.get("timeout_seconds"),
                        "settings": workflow.get("settings"),
                    },
                    timeout=3600,
                )

                if resp.ok:
                    self.event_manager.emit_log(
                        "info",
                        f"Remote workflow execution completed: {execution_id}",
                    )
                else:
                    self.event_manager.emit_log(
                        "error",
                        f"Remote workflow execution failed ({resp.status_code}): {resp.text[:500]}",
                    )

                # Notify via WebSocket that execution has finished
                if self.websocket_handler and self.websocket_handler.is_connected:
                    completion_event = {
                        "type": "execution_completed",
                        "data": {
                            "execution_id": execution_id,
                            "success": resp.ok,
                        },
                    }
                    self.websocket_handler.send_message(json.dumps(completion_event))

            except Exception as e:
                self.event_manager.emit_log(
                    "error", f"Remote workflow execution failed: {e}"
                )

        threading.Thread(target=_run_workflow_background, daemon=True).start()

        return {
            "success": True,
            "execution_id": execution_id,
            "status": "started",
            "message": "Workflow execution started",
        }

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
                state_executor=(
                    self.executor_core.state_executor if self.executor_core else None
                ),
                action_executor=(
                    self.executor_core.action_executor if self.executor_core else None
                ),
                config=self.config,
            )

            # Set screenshot directory if capture is requested
            capture_screenshot = params.get("capture_screenshot", False)
            if capture_screenshot:
                dev_logs_dir = (
                    Path(__file__).parent.parent.parent / ".dev-logs" / "verification"
                )
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
                state_executor=(
                    self.executor_core.state_executor if self.executor_core else None
                ),
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

    def _handle_verification_capture_screenshot(
        self, params: dict[str, Any]
    ) -> dict[str, Any]:
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
                state_executor=(
                    self.executor_core.state_executor if self.executor_core else None
                ),
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
                dev_logs_dir = (
                    Path(__file__).parent.parent.parent
                    / ".dev-logs"
                    / "verification"
                    / "screenshots"
                )
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
            self.event_manager.emit_log(
                "error", f"Failed to capture verification screenshot: {e}"
            )
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

    def _handle_start_interaction_recording(
        self, params: dict[str, Any]
    ) -> dict[str, Any]:
        """Start interaction recording (video + input capture).

        This combines video recording and input monitoring for capturing user
        interactions to be used for State Machine creation.

        Args:
            params: Command parameters:
                - session_id: Optional session ID (auto-generated if not provided)
                - fps: Frames per second for video (default: 30)
                - output_dir: Optional output directory (defaults to .dev-logs/interactions)

        Returns:
            Dictionary with:
                - success: Whether recording started
                - session_id: The session ID being used
                - output_dir: Directory where files will be saved
                - error: Error message (if failed)
        """
        import uuid
        from pathlib import Path

        if self._interaction_recording_active:
            return {
                "success": False,
                "error": "Interaction recording already active",
                "session_id": self._interaction_session_id,
            }

        try:
            # Generate session ID if not provided
            session_id = (
                params.get("session_id") or f"interaction-{uuid.uuid4().hex[:8]}"
            )
            fps = params.get("fps", 30)
            output_dir = params.get("output_dir")

            # Set up output directory
            if output_dir:
                interactions_dir = Path(output_dir)
            else:
                dev_logs_dir = Path(__file__).parent.parent.parent / ".dev-logs"
                interactions_dir = dev_logs_dir / "interactions"

            interactions_dir.mkdir(parents=True, exist_ok=True)

            # Initialize InputMonitorService if not already done
            if self.input_monitor_service is None:
                self.input_monitor_service = InputMonitorService(
                    storage_dir=interactions_dir
                )
                self.event_manager.emit_log(
                    "info", f"InputMonitorService initialized: {interactions_dir}"
                )

            # Start input monitoring
            self.input_monitor_service.start_monitoring(session_id=session_id, fps=fps)
            self.event_manager.emit_log(
                "info", f"Input monitoring started for session: {session_id}"
            )

            # Update state
            self._interaction_recording_active = True
            self._interaction_session_id = session_id
            self._interaction_start_time = time.time()
            self._interaction_fps = fps

            # Emit recording started event (use emit_event_wrapper for string event types)
            self.event_manager.emit_event_wrapper(
                "interaction_recording_started",
                {
                    "session_id": session_id,
                    "fps": fps,
                    "output_dir": str(interactions_dir),
                },
            )

            return {
                "success": True,
                "session_id": session_id,
                "output_dir": str(interactions_dir),
                "fps": fps,
            }

        except Exception as e:
            self.event_manager.emit_log(
                "error", f"Failed to start interaction recording: {e}"
            )
            import traceback

            traceback.print_exc(file=sys.stderr)
            return {"success": False, "error": str(e)}

    def _handle_stop_interaction_recording(self) -> dict[str, Any]:
        """Stop interaction recording and return file paths.

        Returns:
            Dictionary with:
                - success: Whether recording stopped successfully
                - session_id: The session ID
                - events_file: Path to the input events JSONL file
                - events_count: Number of input events captured
                - duration: Recording duration in seconds
                - error: Error message (if failed)
        """
        if not self._interaction_recording_active:
            return {
                "success": False,
                "error": "No interaction recording active",
            }

        try:
            session_id = self._interaction_session_id
            start_time = self._interaction_start_time

            # Stop input monitoring
            events_file = None
            events_count = 0
            if self.input_monitor_service:
                events_file = self.input_monitor_service.stop_monitoring()
                events_count = len(self.input_monitor_service.get_events())

            # Calculate duration
            duration = time.time() - start_time if start_time else 0

            # Reset state
            self._interaction_recording_active = False
            self._interaction_session_id = None
            self._interaction_start_time = None

            self.event_manager.emit_log(
                "info",
                f"Interaction recording stopped: {events_count} events captured, "
                f"duration={duration:.1f}s, file={events_file}",
            )

            # Emit recording stopped event (use emit_event_wrapper for string event types)
            self.event_manager.emit_event_wrapper(
                "interaction_recording_stopped",
                {
                    "session_id": session_id,
                    "events_file": events_file,
                    "events_count": events_count,
                    "duration": duration,
                },
            )

            return {
                "success": True,
                "session_id": session_id,
                "events_file": events_file,
                "events_count": events_count,
                "duration": duration,
            }

        except Exception as e:
            self.event_manager.emit_log(
                "error", f"Failed to stop interaction recording: {e}"
            )
            import traceback

            traceback.print_exc(file=sys.stderr)
            # Reset state even on error
            self._interaction_recording_active = False
            return {"success": False, "error": str(e)}

    def _handle_get_interaction_recording_status(self) -> dict[str, Any]:
        """Get current interaction recording status.

        Returns:
            Dictionary with:
                - success: Always True
                - is_recording: Whether recording is active
                - session_id: Current session ID (if recording)
                - duration: Current recording duration in seconds (if recording)
                - events_count: Number of events captured so far (if recording)
        """
        if not self._interaction_recording_active:
            return {
                "success": True,
                "is_recording": False,
            }

        duration = (
            time.time() - self._interaction_start_time
            if self._interaction_start_time
            else 0
        )
        events_count = 0
        if self.input_monitor_service:
            events_count = len(self.input_monitor_service.get_events())

        return {
            "success": True,
            "is_recording": True,
            "session_id": self._interaction_session_id,
            "duration": duration,
            "events_count": events_count,
            "fps": self._interaction_fps,
        }

    # =========================================================================
    # Click-to-Template Capture Commands
    # =========================================================================

    def _handle_start_click_capture(self, params: dict[str, Any]) -> dict[str, Any]:
        """Start click capture for template extraction.

        This captures input events (clicks) that will later be processed
        to extract template candidates using the qontinui library.

        Args:
            params: Command parameters:
                - session_id: Optional session ID (auto-generated if not provided)
                - output_dir: Optional output directory
                - application_hint: Optional application name for profile lookup

        Returns:
            Dictionary with:
                - success: Whether capture started
                - session_id: The session ID being used
                - output_dir: Directory where files will be saved
                - error: Error message (if failed)
        """
        import uuid
        from pathlib import Path

        if self._click_capture_active:
            return {
                "success": False,
                "error": "Click capture already active",
                "session_id": self._click_capture_session_id,
            }

        try:
            # Generate session ID if not provided
            session_id = (
                params.get("session_id") or f"click-capture-{uuid.uuid4().hex[:8]}"
            )
            output_dir = params.get("output_dir")
            application_hint = params.get("application_hint")

            # Set up output directory
            if output_dir:
                capture_dir = Path(output_dir)
            else:
                dev_logs_dir = Path(__file__).parent.parent.parent / ".dev-logs"
                capture_dir = dev_logs_dir / "click-captures"

            capture_dir.mkdir(parents=True, exist_ok=True)

            # Initialize InputMonitorService if not already done
            if self.input_monitor_service is None:
                self.input_monitor_service = InputMonitorService(
                    storage_dir=capture_dir
                )
                self.event_manager.emit_log(
                    "info",
                    f"InputMonitorService initialized for click capture: {capture_dir}",
                )

            # Start input monitoring (captures clicks)
            self.input_monitor_service.start_monitoring(session_id=session_id, fps=30)
            self.event_manager.emit_log(
                "info", f"Click capture started for session: {session_id}"
            )

            # Update state
            self._click_capture_active = True
            self._click_capture_session_id = session_id
            self._click_capture_start_time = time.time()
            self._click_capture_output_dir = str(capture_dir)
            self._click_capture_application_hint = application_hint

            # Emit capture started event
            self.event_manager.emit_event_wrapper(
                "click_capture_started",
                {
                    "session_id": session_id,
                    "output_dir": str(capture_dir),
                    "application_hint": application_hint,
                },
            )

            return {
                "success": True,
                "session_id": session_id,
                "output_dir": str(capture_dir),
                "application_hint": application_hint,
            }

        except Exception as e:
            self.event_manager.emit_log("error", f"Failed to start click capture: {e}")
            import traceback

            traceback.print_exc(file=sys.stderr)
            return {"success": False, "error": str(e)}

    def _handle_stop_click_capture(self, params: dict[str, Any]) -> dict[str, Any]:
        """Stop click capture and optionally process results.

        Args:
            params: Command parameters:
                - process_immediately: Whether to process captures now (default: False)

        Returns:
            Dictionary with:
                - success: Whether capture stopped successfully
                - session_id: The session ID
                - events_file: Path to the input events JSONL file
                - events_count: Number of click events captured
                - duration: Capture duration in seconds
                - candidates: Template candidates (if process_immediately=True)
                - error: Error message (if failed)
        """
        if not self._click_capture_active:
            return {
                "success": False,
                "error": "No click capture active",
            }

        try:
            session_id = self._click_capture_session_id
            start_time = self._click_capture_start_time
            output_dir = self._click_capture_output_dir
            application_hint = self._click_capture_application_hint
            process_immediately = params.get("process_immediately", False)

            # Stop input monitoring
            events_file = None
            events_count = 0
            if self.input_monitor_service:
                events_file = self.input_monitor_service.stop_monitoring()
                # Filter for click events only
                all_events = self.input_monitor_service.get_events()
                click_events = [
                    e for e in all_events if e.get("event_type") == "mouse_click"
                ]
                events_count = len(click_events)

            # Calculate duration
            duration = time.time() - start_time if start_time else 0

            # Reset state
            self._click_capture_active = False
            self._click_capture_session_id = None
            self._click_capture_start_time = None
            self._click_capture_output_dir = None
            self._click_capture_application_hint = None

            self.event_manager.emit_log(
                "info",
                f"Click capture stopped: {events_count} click events captured, "
                f"duration={duration:.1f}s, file={events_file}",
            )

            result = {
                "success": True,
                "session_id": session_id,
                "events_file": events_file,
                "events_count": events_count,
                "duration": duration,
                "output_dir": output_dir,
            }

            # Process immediately if requested
            if process_immediately and events_file:
                process_result = self._process_click_capture_internal(
                    events_file=events_file,
                    session_id=session_id,
                    application_hint=application_hint,
                )
                result["candidates"] = process_result.get("candidates", [])
                result["candidates_count"] = process_result.get("candidates_count", 0)

            # Emit capture stopped event
            self.event_manager.emit_event_wrapper(
                "click_capture_stopped",
                {
                    "session_id": session_id,
                    "events_file": events_file,
                    "events_count": events_count,
                    "duration": duration,
                },
            )

            return result

        except Exception as e:
            self.event_manager.emit_log("error", f"Failed to stop click capture: {e}")
            import traceback

            traceback.print_exc(file=sys.stderr)
            # Reset state even on error
            self._click_capture_active = False
            return {"success": False, "error": str(e)}

    def _handle_get_click_capture_status(self) -> dict[str, Any]:
        """Get current click capture status.

        Returns:
            Dictionary with:
                - success: Always True
                - is_capturing: Whether capture is active
                - session_id: Current session ID (if capturing)
                - duration: Current capture duration in seconds (if capturing)
                - events_count: Number of click events captured so far (if capturing)
        """
        if not self._click_capture_active:
            return {
                "success": True,
                "is_capturing": False,
            }

        duration = (
            time.time() - self._click_capture_start_time
            if self._click_capture_start_time
            else 0
        )
        events_count = 0
        if self.input_monitor_service:
            all_events = self.input_monitor_service.get_events()
            click_events = [
                e for e in all_events if e.get("event_type") == "mouse_click"
            ]
            events_count = len(click_events)

        return {
            "success": True,
            "is_capturing": True,
            "session_id": self._click_capture_session_id,
            "duration": duration,
            "events_count": events_count,
            "output_dir": self._click_capture_output_dir,
            "application_hint": self._click_capture_application_hint,
        }

    def _handle_process_click_capture(self, params: dict[str, Any]) -> dict[str, Any]:
        """Process a completed click capture session to extract template candidates.

        Uses the qontinui library's CaptureProcessor to analyze screenshots
        at click timestamps and detect element boundaries.

        Args:
            params: Command parameters:
                - events_file: Path to the events JSONL file (required)
                - video_path: Path to the video file (optional - uses screenshots if not provided)
                - session_id: Session ID for the candidates
                - application_hint: Optional application name for profile lookup
                - send_to_web: Whether to send candidates to qontinui-web API (default: False)
                - web_api_url: URL for qontinui-web API (required if send_to_web=True)

        Returns:
            Dictionary with:
                - success: Whether processing succeeded
                - candidates_count: Number of template candidates extracted
                - candidates: List of candidate dictionaries
                - error: Error message (if failed)
        """
        events_file = params.get("events_file")
        if not events_file:
            return {"success": False, "error": "events_file is required"}

        return self._process_click_capture_internal(
            events_file=events_file,
            video_path=params.get("video_path"),
            session_id=params.get("session_id"),
            application_hint=params.get("application_hint"),
            send_to_web=params.get("send_to_web", False),
            web_api_url=params.get("web_api_url"),
        )

    def _process_click_capture_internal(
        self,
        events_file: str,
        video_path: str | None = None,
        session_id: str | None = None,
        application_hint: str | None = None,
        send_to_web: bool = False,
        web_api_url: str | None = None,
    ) -> dict[str, Any]:
        """Internal method to process click capture.

        If video_path is provided, extracts frames from video at click timestamps.
        Otherwise, captures screenshots at the current time for each click location.
        """
        import json
        import uuid
        from pathlib import Path

        try:
            # Import qontinui library modules
            from qontinui.discovery.click_analysis import (
                CaptureProcessor,
                ClickTemplateCandidate,
            )

            events_path = Path(events_file)
            if not events_path.exists():
                return {
                    "success": False,
                    "error": f"Events file not found: {events_file}",
                }

            # Load click events from JSONL
            click_events = []
            with open(events_path) as f:
                for line in f:
                    line = line.strip()
                    if not line:
                        continue
                    try:
                        event = json.loads(line)
                        if event.get("event_type") == "mouse_click":
                            click_events.append(event)
                    except json.JSONDecodeError:
                        continue

            if not click_events:
                return {
                    "success": True,
                    "candidates_count": 0,
                    "candidates": [],
                    "message": "No click events found in file",
                }

            self.event_manager.emit_log(
                "info",
                f"Processing {len(click_events)} click events from {events_file}",
            )

            processor = CaptureProcessor()
            candidates: list[ClickTemplateCandidate] = []

            if video_path and Path(video_path).exists():
                # Process from video file
                candidates = processor.process_capture_session(
                    video_path=Path(video_path),
                    events_file=events_path,
                    session_id=session_id or str(uuid.uuid4()),
                    application_hint=application_hint,
                )
            else:
                # Process from current screenshots (for each click location)
                # Take a screenshot now and process all clicks against it
                screenshot = self._capture_screenshot_for_processing()
                if screenshot is not None:
                    click_locations = [
                        (e.get("x", 0), e.get("y", 0)) for e in click_events
                    ]
                    candidates = processor.process_screenshot_with_clicks(
                        screenshot=screenshot,
                        click_locations=click_locations,
                        session_id=session_id or str(uuid.uuid4()),
                        application_hint=application_hint,
                    )

            # Convert candidates to dictionaries
            candidate_dicts = [c.to_dict() for c in candidates]

            self.event_manager.emit_log(
                "info", f"Extracted {len(candidates)} template candidates"
            )

            result = {
                "success": True,
                "candidates_count": len(candidates),
                "candidates": candidate_dicts,
            }

            # Send to web API if requested
            if send_to_web and web_api_url and candidates:
                send_result = self._send_candidates_to_web(candidate_dicts, web_api_url)
                result["web_send_success"] = send_result.get("success", False)
                if not send_result.get("success"):
                    result["web_send_error"] = send_result.get("error")

            return result

        except ImportError as e:
            self.event_manager.emit_log(
                "error", f"Failed to import qontinui library: {e}"
            )
            return {"success": False, "error": f"qontinui library not available: {e}"}
        except Exception as e:
            self.event_manager.emit_log(
                "error", f"Failed to process click capture: {e}"
            )
            import traceback

            traceback.print_exc(file=sys.stderr)
            return {"success": False, "error": str(e)}

    def _capture_screenshot_for_processing(self):
        """Capture a screenshot for click capture processing."""
        try:
            if QONTINUI_AVAILABLE:
                from qontinui.hal import create_default_hal

                hal = create_default_hal()
                screenshot = hal.capture_screen()
                return screenshot
        except Exception as e:
            self.event_manager.emit_log(
                "warning", f"Failed to capture screenshot for processing: {e}"
            )
        return None

    def _send_candidates_to_web(
        self, candidates: list[dict[str, Any]], web_api_url: str
    ) -> dict[str, Any]:
        """Send template candidates to qontinui-web API."""
        import requests

        try:
            response = requests.post(
                f"{web_api_url}/api/v1/template-capture/candidates",
                json=candidates,
                timeout=30,
            )
            response.raise_for_status()
            return {"success": True}
        except Exception as e:
            self.event_manager.emit_log(
                "error", f"Failed to send candidates to web API: {e}"
            )
            return {"success": False, "error": str(e)}

    def _handle_generate_state_machine(self, params: dict[str, Any]) -> dict[str, Any]:
        """Generate a state machine configuration from approved templates.

        Uses the qontinui library's ClickToStateMachineBuilder to convert
        user-approved template candidates into a state machine configuration.

        Args:
            params: Command parameters:
                - approved_templates: List of approved template dictionaries (required)
                - grouping_method: How to group templates into states. Options:
                    - "state_hints": Use template's state_hint field (default)
                    - "user_assignments": Use explicit state_assignments mapping
                    - "co_occurrence": Group by which templates appear together
                    - "single_state": Put all templates in one state
                    - "one_per_template": Each template becomes its own state
                - state_assignments: Dict mapping state_name to list of template IDs
                    (required if grouping_method="user_assignments")
                - session_id: Session ID for metadata (optional)
                - video_path: Path to video file used for capture (optional, but
                    REQUIRED for co_occurrence grouping to analyze which templates
                    appear together)
                - co_occurrence_sample_interval: For co_occurrence grouping, check
                    every N frames (default: 30, ~1 per second at 30fps)
                - output_path: Path to write the config JSON file (optional)

        Returns:
            Dictionary with:
                - success: Whether generation succeeded
                - state_machine: The generated state machine configuration dict
                - states_count: Number of states in the generated config
                - state_images_count: Total number of state images
                - transitions_count: Number of transitions
                - output_path: Path where config was saved (if output_path provided)
                - error: Error message (if failed)
        """
        from pathlib import Path

        approved_templates_data = params.get("approved_templates")
        if not approved_templates_data:
            return {"success": False, "error": "approved_templates is required"}

        if not isinstance(approved_templates_data, list):
            return {"success": False, "error": "approved_templates must be a list"}

        grouping_method = params.get("grouping_method", "state_hints")
        state_assignments = params.get("state_assignments")
        session_id = params.get("session_id", "")
        video_path = params.get("video_path")
        output_path = params.get("output_path")
        co_occurrence_sample_interval = params.get("co_occurrence_sample_interval", 30)

        # Validate grouping method
        valid_methods = [
            "state_hints",
            "user_assignments",
            "co_occurrence",
            "single_state",
            "one_per_template",
        ]
        if grouping_method not in valid_methods:
            return {
                "success": False,
                "error": f"Invalid grouping_method. Must be one of: {valid_methods}",
            }

        # Validate state_assignments if using user_assignments method
        if grouping_method == "user_assignments" and not state_assignments:
            return {
                "success": False,
                "error": "state_assignments is required when grouping_method='user_assignments'",
            }

        # Validate video_path if using co_occurrence method
        if grouping_method == "co_occurrence" and not video_path:
            return {
                "success": False,
                "error": "video_path is required when grouping_method='co_occurrence'. "
                "The video is analyzed to determine which templates appear together.",
            }

        try:
            # Import qontinui library modules
            from qontinui.discovery.click_analysis import (
                ApprovedTemplate,
                ClickToStateMachineBuilder,
            )

            # Convert dictionaries to ApprovedTemplate objects
            approved_templates: list[ApprovedTemplate] = []
            for template_data in approved_templates_data:
                try:
                    template = ApprovedTemplate.from_dict(template_data)
                    approved_templates.append(template)
                except Exception as e:
                    self.event_manager.emit_log(
                        "warning",
                        f"Failed to parse template {template_data.get('id', 'unknown')}: {e}",
                    )
                    continue

            if not approved_templates:
                return {
                    "success": False,
                    "error": "No valid templates could be parsed from approved_templates",
                }

            self.event_manager.emit_log(
                "info",
                f"Building state machine from {len(approved_templates)} approved templates "
                f"using {grouping_method} grouping",
            )

            # Emit progress event
            self.event_manager.emit_event_wrapper(
                "state_machine_generation_started",
                {
                    "template_count": len(approved_templates),
                    "grouping_method": grouping_method,
                    "session_id": session_id,
                },
            )

            # Build the state machine
            builder = ClickToStateMachineBuilder()
            result = builder.build_from_templates(
                templates=approved_templates,
                grouping_method=grouping_method,
                state_assignments=state_assignments,
                session_id=session_id,
                video_path=Path(video_path) if video_path else None,
                co_occurrence_sample_interval=co_occurrence_sample_interval,
            )

            # Convert to dictionary for JSON serialization
            state_machine_dict = result.to_dict()

            self.event_manager.emit_log(
                "info",
                f"Generated state machine: {result.state_count} states, "
                f"{result.state_image_count} state images, {result.transition_count} transitions",
            )

            # Write to file if output_path provided
            if output_path:
                import json

                output_file = Path(output_path)
                output_file.parent.mkdir(parents=True, exist_ok=True)
                with open(output_file, "w") as f:
                    json.dump(state_machine_dict, f, indent=2)
                self.event_manager.emit_log(
                    "info", f"State machine config saved to: {output_path}"
                )

            # Emit completion event
            self.event_manager.emit_event_wrapper(
                "state_machine_generation_completed",
                {
                    "states_count": result.state_count,
                    "state_images_count": result.state_image_count,
                    "transitions_count": result.transition_count,
                    "session_id": session_id,
                    "output_path": output_path,
                },
            )

            return {
                "success": True,
                "state_machine": state_machine_dict,
                "states_count": result.state_count,
                "state_images_count": result.state_image_count,
                "transitions_count": result.transition_count,
                "output_path": output_path,
            }

        except ImportError as e:
            self.event_manager.emit_log(
                "error", f"Failed to import qontinui library: {e}"
            )
            return {"success": False, "error": f"qontinui library not available: {e}"}
        except Exception as e:
            self.event_manager.emit_log(
                "error", f"Failed to generate state machine: {e}"
            )
            import traceback

            traceback.print_exc(file=sys.stderr)
            return {"success": False, "error": str(e)}

    # =========================================================================
    # UI Bridge State Machine handlers
    # =========================================================================

    @staticmethod
    def _normalize_sm_config(config_data: dict[str, Any]) -> dict[str, Any]:
        """Normalize state machine config from CRUD API format to from_dict format.

        The CRUD API returns states/transitions as lists with database fields
        (state_id, extra_metadata, config_id, etc). UIBridgeRuntime.from_dict
        expects dicts keyed by ID with UIBridgeState/UIBridgeTransition fields.
        """
        result: dict[str, Any] = {}

        # Transform states: list -> dict keyed by state_id
        states = config_data.get("states", {})
        if isinstance(states, list):
            result["states"] = {}
            for s in states:
                sid = s.get("state_id", s.get("id", ""))
                result["states"][sid] = {
                    "id": sid,
                    "name": s.get("name", sid),
                    "element_ids": s.get("element_ids", []),
                    "metadata": s.get("extra_metadata", s.get("metadata", {})),
                }
        else:
            result["states"] = states

        # Transform transitions: list -> dict keyed by transition_id
        transitions = config_data.get("transitions", {})
        if isinstance(transitions, list):
            result["transitions"] = {}
            for t in transitions:
                tid = t.get("transition_id", t.get("id", ""))
                result["transitions"][tid] = {
                    "id": tid,
                    "name": t.get("name", tid),
                    "from_states": t.get("from_states", []),
                    "activate_states": t.get("activate_states", []),
                    "exit_states": t.get("exit_states", []),
                    "actions": t.get("actions", []),
                    "path_cost": t.get("path_cost", 1.0),
                    "stays_visible": t.get("stays_visible", False),
                    "metadata": t.get("extra_metadata", t.get("metadata", {})),
                }
        else:
            result["transitions"] = transitions

        if "config" in config_data:
            result["config"] = config_data["config"]

        return result

    def _handle_load_state_machine(self, params: dict[str, Any]) -> dict[str, Any]:
        """Load a UI Bridge state machine configuration.

        Creates a UIBridgeRuntime from the exported config and stores it
        in memory for subsequent operations. Persists to SQLite for
        auto-reload on restart.

        Args:
            params: Must contain "config" key with the exported state machine JSON.
        """
        config_data = params.get("config")
        if not config_data:
            return {"success": False, "error": "config is required"}

        try:
            from element_resolver import ElementResolver
            from ui_bridge_http_client import (
                ResolvingUIBridgeClient,
                UIBridgeHTTPClient,
            )

            from qontinui.state_machine.ui_bridge_runtime import UIBridgeRuntime

            persistence = self._get_sm_persistence()

            # Create HTTP client with resolving wrapper
            inner_client = UIBridgeHTTPClient(self._get_runner_api_base())
            resolver = ElementResolver(persistence)
            client = ResolvingUIBridgeClient(inner_client, resolver)

            # Normalize CRUD API format to from_dict format if needed
            normalized = self._normalize_sm_config(config_data)
            runtime = UIBridgeRuntime.from_dict(normalized, client)
            self._ui_bridge_runtime = runtime
            self._element_resolver = resolver

            # Persist config for auto-reload on restart
            persistence.save_config(config_data)

            # Capture element snapshots for cross-session ID mapping
            try:
                elements = inner_client.find().elements
                resolver.capture_snapshots(elements)
            except Exception:
                pass  # UI Bridge may not be connected yet

            stats = runtime.get_statistics()
            self.event_manager.emit_log(
                "info",
                f"State machine loaded: {stats['states']['registered']} states, "
                f"{stats['transitions']['registered']} transitions",
            )
            return {"success": True, "statistics": stats}

        except ImportError as e:
            return {"success": False, "error": f"Required library not available: {e}"}
        except Exception as e:
            self.event_manager.emit_log("error", f"Failed to load state machine: {e}")
            return {"success": False, "error": str(e)}

    def _handle_get_state_machine_status(self) -> dict[str, Any]:
        """Get the status of the loaded state machine."""
        if self._ui_bridge_runtime is None:
            return {
                "success": True,
                "loaded": False,
                "message": "No state machine loaded",
            }

        try:
            stats = self._ui_bridge_runtime.get_statistics()
            return {
                "success": True,
                "loaded": True,
                "statistics": stats,
            }
        except Exception as e:
            return {"success": False, "error": str(e)}

    def _handle_sm_execute_transition(self, params: dict[str, Any]) -> dict[str, Any]:
        """Execute a specific transition by ID."""
        if self._ui_bridge_runtime is None:
            return {"success": False, "error": "No state machine loaded"}

        transition_id = params.get("transition_id")
        if not transition_id:
            return {"success": False, "error": "transition_id is required"}

        try:
            result = self._ui_bridge_runtime.execute_transition(transition_id)
            return {
                "success": True,
                "transition_id": transition_id,
                "result": result if isinstance(result, dict) else {"completed": True},
            }
        except Exception as e:
            self.event_manager.emit_log(
                "error", f"Failed to execute transition {transition_id}: {e}"
            )
            return {"success": False, "error": str(e)}

    def _handle_sm_navigate_to_states(self, params: dict[str, Any]) -> dict[str, Any]:
        """Navigate to target states using pathfinding."""
        if self._ui_bridge_runtime is None:
            return {"success": False, "error": "No state machine loaded"}

        target_states = params.get("target_states")
        if not target_states:
            return {"success": False, "error": "target_states is required"}

        try:
            result = self._ui_bridge_runtime.navigate_to(target_states)
            return {
                "success": True,
                "target_states": target_states,
                "result": result if isinstance(result, dict) else {"completed": True},
            }
        except Exception as e:
            self.event_manager.emit_log(
                "error", f"Failed to navigate to states {target_states}: {e}"
            )
            return {"success": False, "error": str(e)}

    def _handle_sm_get_active_states(self) -> dict[str, Any]:
        """Get currently active states from the runtime."""
        if self._ui_bridge_runtime is None:
            return {"success": False, "error": "No state machine loaded"}

        try:
            active_states = self._ui_bridge_runtime.get_active_states()
            return {
                "success": True,
                "active_states": active_states,
            }
        except Exception as e:
            return {"success": False, "error": str(e)}

    def _handle_sm_get_available_transitions(self) -> dict[str, Any]:
        """Get transitions available from the current state."""
        if self._ui_bridge_runtime is None:
            return {"success": False, "error": "No state machine loaded"}

        try:
            transitions = self._ui_bridge_runtime.get_available_transitions()
            # Convert transition objects to dicts for JSON serialization
            transition_list: list[Any] = []
            for t in transitions:
                if hasattr(t, "__dict__"):
                    transition_list.append(
                        {
                            "id": getattr(t, "id", None),
                            "name": getattr(t, "name", None),
                            "from_states": getattr(t, "from_states", []),
                            "activate_states": getattr(t, "activate_states", []),
                            "exit_states": getattr(t, "exit_states", []),
                        }
                    )
                elif isinstance(t, dict):
                    transition_list.append(t)
                else:
                    transition_list.append(str(t))

            return {
                "success": True,
                "transitions": transition_list,
            }
        except Exception as e:
            return {"success": False, "error": str(e)}

    def _handle_clear_state_machine(self) -> dict[str, Any]:
        """Clear the loaded state machine and persisted data."""
        was_loaded = self._ui_bridge_runtime is not None
        self._ui_bridge_runtime = None
        self._element_resolver = None

        # Clear persisted data
        try:
            persistence = self._get_sm_persistence()
            persistence.clear()
        except Exception:
            pass

        self.event_manager.emit_log(
            "info",
            "State machine cleared" if was_loaded else "No state machine was loaded",
        )
        return {"success": True, "was_loaded": was_loaded}

    # =========================================================================
    # GUI Config Pipeline commands
    # =========================================================================

    def _fetch_ui_bridge_snapshot(self, api_port: int) -> dict[str, Any] | None:
        """Fetch a UI Bridge snapshot via HTTP.

        Returns the snapshot dict (with 'elements' key) or None on failure.
        On failure, logs the error and returns None.
        """
        import http.client
        import json as _json
        import sys

        try:
            conn = http.client.HTTPConnection("127.0.0.1", api_port, timeout=30)
            conn.request("GET", "/ui-bridge/control/snapshot")
            resp = conn.getresponse()
            body = resp.read().decode("utf-8")
            conn.close()
            if resp.status != 200:
                print(
                    f"[warn    ] EXECUTOR: UI Bridge snapshot returned status {resp.status}",
                    file=sys.stderr,
                    flush=True,
                )
                return None
            snapshot_response = _json.loads(body)
        except Exception as e:
            print(
                f"[warn    ] EXECUTOR: Failed to fetch UI Bridge snapshot: {e}",
                file=sys.stderr,
                flush=True,
            )
            return None

        # The HTTP endpoint wraps in {success, data: {elements, ...}}
        snapshot = snapshot_response.get("data", snapshot_response)
        element_count = len(snapshot.get("elements", []))
        print(
            f"[info    ] EXECUTOR: Got {element_count} elements from UI Bridge",
            file=sys.stderr,
            flush=True,
        )
        return snapshot

    def _handle_gui_config_capture_elements(
        self, params: dict[str, Any]
    ) -> dict[str, Any]:
        """Extract element images using UI Bridge DOM capture.

        Captures element images directly from the webview DOM via html2canvas,
        bypassing MSS screen capture entirely. This produces correct images
        regardless of window z-order (other windows can cover the runner).

        Args:
            params:
                - api_port: Runner API port for UI Bridge
                - category_filter: Optional list of categories to include
                - min_element_size: Minimum element dimension in pixels (default 4)
                - scale_factor: DPI scale factor (default 1.0, used for filtering only)
                - padding: Extra pixels around each crop (default 0, unused with DOM capture)

        Returns:
            Dictionary with element_images mapping and metadata.
        """
        import sys

        try:
            if not QONTINUI_AVAILABLE:
                return {"success": False, "error": "Qontinui library not available"}

            from qontinui.discovery.element_image_pipeline import (
                ElementImagePipeline,
                ExtractionConfig,
            )

            api_port = params.get("api_port", 9876)
            snapshot = self._fetch_ui_bridge_snapshot(api_port)
            if snapshot is None:
                return {"success": False, "error": "Failed to fetch UI Bridge snapshot"}
            if not snapshot.get("elements"):
                return {"success": False, "error": "UI Bridge snapshot has no elements"}

            # Capture element images directly from the DOM
            captures = self._fetch_ui_bridge_element_captures(api_port)
            if captures is None:
                return {
                    "success": False,
                    "error": "Failed to capture element images via UI Bridge",
                }

            # Build config (filters still apply)
            cat_filter = params.get("category_filter")
            config = ExtractionConfig(
                min_element_size=params.get("min_element_size", 4),
                padding=params.get("padding", 0),
                scale_factor=params.get("scale_factor", 1.0),
                category_filter=set(cat_filter) if cat_filter else None,
            )

            pipeline = ElementImagePipeline(config)
            result = pipeline.extract_from_captures(snapshot, captures)

            # Build response — element images as a dict
            element_images = {}
            for img in result.images:
                element_images[img.element_id] = {
                    "base64_png": img.base64_png,
                    "width": img.width,
                    "height": img.height,
                    "sha256": img.sha256,
                    "label": img.label,
                    "type": img.element_type,
                    "bbox": list(img.bbox),
                }

            return {
                "success": True,
                "element_count": len(result.images),
                "skipped_count": len(result.skipped),
                "screenshot_size": [result.screenshot_width, result.screenshot_height],
                "viewport_size": [result.viewport_width, result.viewport_height],
                "element_images": element_images,
                "skipped": result.skipped,
            }

        except Exception as e:
            print(
                f"[error   ] EXECUTOR: gui_config_capture_elements failed: {e}",
                file=sys.stderr,
                flush=True,
            )
            import traceback

            traceback.print_exc(file=sys.stderr)
            return {"success": False, "error": str(e)}

    def _handle_gui_config_build(self, params: dict[str, Any]) -> dict[str, Any]:
        """Build a QontinuiConfig from element images and state/transition definitions.

        Args:
            params:
                - name: Config name
                - states: List of state dicts with id, name, element_ids
                - transitions: List of transition dicts
                - element_images: Dict of element_id -> image data
                - description: Optional description
                - similarity: Default similarity threshold (default 0.85)

        Returns:
            Dictionary with the complete QontinuiConfig.
        """
        import sys

        try:
            if not QONTINUI_AVAILABLE:
                return {"success": False, "error": "Qontinui library not available"}

            import base64
            import io

            from PIL import Image

            from qontinui.discovery.element_image_pipeline import (
                ElementRect,
                ExtractedElementImage,
            )
            from qontinui.state_machine.config_bridge import (
                ConfigBridge,
                UIBridgeStateInput,
                UIBridgeTransitionInput,
            )

            name = params.get("name", "Untitled Config")
            states_raw = params.get("states", [])
            transitions_raw = params.get("transitions", [])
            element_images_raw = params.get("element_images", {})
            description = params.get("description", "")
            similarity = float(params.get("similarity", 0.85))

            if not states_raw:
                return {"success": False, "error": "states is required"}
            if not element_images_raw:
                return {"success": False, "error": "element_images is required"}

            # Convert raw states
            ui_states = [
                UIBridgeStateInput(
                    id=s["id"],
                    name=s["name"],
                    element_ids=s.get("element_ids", []),
                    description=s.get("description", ""),
                    is_initial=s.get("is_initial", False),
                    is_final=s.get("is_final", False),
                )
                for s in states_raw
            ]

            # Convert raw transitions
            ui_transitions = [
                UIBridgeTransitionInput(
                    id=t["id"],
                    name=t["name"],
                    from_states=t.get("from_states", []),
                    activate_states=t.get("activate_states", []),
                    exit_states=t.get("exit_states", []),
                    stays_visible=t.get("stays_visible", False),
                )
                for t in transitions_raw
            ]

            # Reconstruct ExtractedElementImage objects
            all_element_images: dict[str, ExtractedElementImage] = {}
            for eid, data in element_images_raw.items():
                b64 = data.get("base64_png", "")
                if not b64:
                    continue
                img = Image.open(io.BytesIO(base64.b64decode(b64)))
                bbox = data.get("bbox", [0, 0, img.width, img.height])
                all_element_images[eid] = ExtractedElementImage(
                    element_id=eid,
                    label=data.get("label", eid),
                    element_type=data.get("type", "unknown"),
                    image=img,
                    bbox=(int(bbox[0]), int(bbox[1]), int(bbox[2]), int(bbox[3])),
                    base64_png=b64,
                    sha256=data.get("sha256", ""),
                    viewport_rect=ElementRect(
                        x=0, y=0, width=float(img.width), height=float(img.height)
                    ),
                )

            # Group images by state
            state_images: dict[str, list[ExtractedElementImage]] = {}
            for s in ui_states:
                state_images[s.id] = [
                    all_element_images[eid]
                    for eid in s.element_ids
                    if eid in all_element_images
                ]

            # Build config
            bridge = ConfigBridge(default_similarity=similarity)
            config = bridge.build_config(
                name=name,
                states=ui_states,
                transitions=ui_transitions,
                state_images=state_images,
                description=description,
            )

            return {
                "success": True,
                "config": config,
                "stats": {
                    "image_count": len(config.get("images", [])),
                    "state_count": len(config.get("states", [])),
                    "transition_count": len(config.get("transitions", [])),
                },
            }

        except Exception as e:
            print(
                f"[error   ] EXECUTOR: gui_config_build failed: {e}",
                file=sys.stderr,
                flush=True,
            )
            import traceback

            traceback.print_exc(file=sys.stderr)
            return {"success": False, "error": str(e)}

    def _handle_gui_config_capture_multi_state(
        self, params: dict[str, Any]
    ) -> dict[str, Any]:
        """Orchestrate multi-state GUI config capture using UI Bridge DOM capture.

        Walks through a sequence of interactions, capturing element images
        directly from the DOM via html2canvas at each step. Diffs element sets
        to find only NEW elements per state, and builds a complete QontinuiConfig.

        No MSS screen capture is used — element images come from the live DOM,
        so other windows covering the runner do not affect the result.

        Args:
            params:
                - api_port: Runner API port for UI Bridge
                - name: Config name
                - interactions: List of {action_type, target, state_name, wait_seconds}
                - min_element_size: Minimum element dimension (default 4)
                - description: Optional config description
                - similarity: Default similarity threshold (default 0.85)
                - scale_factor: DPI scale (default 1.0, used for filtering only)

        Returns:
            Dictionary with complete QontinuiConfig and capture stats.
        """
        import sys

        try:
            if not QONTINUI_AVAILABLE:
                return {"success": False, "error": "Qontinui library not available"}

            import time

            from qontinui.discovery.element_image_pipeline import (
                ElementImagePipeline,
                ExtractionConfig,
                ExtractedElementImage,
            )
            from qontinui.state_machine.config_bridge import (
                ConfigBridge,
                UIBridgeStateInput,
                UIBridgeTransitionInput,
            )

            api_port = params.get("api_port", 9876)
            scale = params.get("scale_factor", 1.0)
            interactions = params.get("interactions", [])
            min_size = params.get("min_element_size", 4)
            name = params.get("name", "Untitled Multi-State Config")
            description = params.get("description", "")
            similarity = float(params.get("similarity", 0.85))

            if not interactions:
                return {"success": False, "error": "interactions list is required"}

            config = ExtractionConfig(
                min_element_size=min_size,
                padding=0,
                scale_factor=scale,
            )
            pipeline = ElementImagePipeline(config)

            seen_ids: set[str] = set()
            ui_states: list[UIBridgeStateInput] = []
            ui_transitions: list[UIBridgeTransitionInput] = []
            state_images: dict[str, list[ExtractedElementImage]] = {}

            for i, interaction in enumerate(interactions):
                action_type = interaction.get("action_type", "initial")
                target = interaction.get("target")
                state_name = interaction.get("state_name", f"State {i}")
                wait_seconds = float(interaction.get("wait_seconds", 1.0))

                print(
                    f"[info    ] EXECUTOR: Multi-state step {i}: {action_type}"
                    f" target={target} state={state_name}",
                    file=sys.stderr,
                    flush=True,
                )

                # 1. Perform action (if not initial)
                if action_type != "initial" and target:
                    self._perform_ui_bridge_action(api_port, action_type, target)
                    time.sleep(wait_seconds)

                # 2. Fetch UI Bridge snapshot
                snapshot = self._fetch_ui_bridge_snapshot(api_port)
                if snapshot is None:
                    continue
                elements = snapshot.get("elements", [])
                if not elements:
                    print(
                        f"[warn    ] EXECUTOR: No elements at step {i}",
                        file=sys.stderr,
                        flush=True,
                    )
                    continue

                # 3. Diff: find only NEW element IDs
                current_ids: set[str] = set()
                for el in elements:
                    eid = el.get("id", "")
                    rect = el.get("state", {}).get("rect", {})
                    w = rect.get("width", 0)
                    h = rect.get("height", 0)
                    visible = el.get("state", {}).get("visible", True)
                    if visible and w >= min_size and h >= min_size and eid:
                        current_ids.add(eid)

                new_ids = current_ids - seen_ids
                seen_ids |= current_ids

                if not new_ids:
                    print(
                        f"[info    ] EXECUTOR: No new elements at step {i}, skipping",
                        file=sys.stderr,
                        flush=True,
                    )
                    continue

                print(
                    f"[info    ] EXECUTOR: Step {i}: {len(new_ids)} new elements"
                    f" (total seen: {len(seen_ids)})",
                    file=sys.stderr,
                    flush=True,
                )

                # 4. Capture only the new elements via UI Bridge DOM capture
                captures = self._fetch_ui_bridge_element_captures(
                    api_port, element_ids=list(new_ids)
                )
                if captures is None:
                    print(
                        f"[warn    ] EXECUTOR: DOM capture failed at step {i}, skipping",
                        file=sys.stderr,
                        flush=True,
                    )
                    continue

                # 5. Filter snapshot to only new elements, run pipeline
                filtered_elements = [
                    el for el in elements if el.get("id", "") in new_ids
                ]
                filtered_snapshot = {**snapshot, "elements": filtered_elements}

                result = pipeline.extract_from_captures(
                    filtered_snapshot, captures
                )

                # 6. Record state
                state_id = f"state-{i}"
                ui_states.append(
                    UIBridgeStateInput(
                        id=state_id,
                        name=state_name,
                        element_ids=list(new_ids),
                        is_initial=(i == 0),
                    )
                )
                state_images[state_id] = result.images

                # 7. Record transition (if not initial and we have a previous state)
                if action_type != "initial" and len(ui_states) >= 2:
                    prev_state = ui_states[-2]
                    ui_transitions.append(
                        UIBridgeTransitionInput(
                            id=f"transition-{i}",
                            name=f"{action_type.title()} {target or ''}".strip(),
                            from_states=[prev_state.id],
                            activate_states=[state_id],
                            exit_states=[],
                            actions=[
                                {
                                    "type": action_type,
                                    "target": target,
                                }
                            ],
                        )
                    )

            if not ui_states:
                return {
                    "success": False,
                    "error": "No states captured — no new elements found at any step",
                }

            # 8. Build final config
            bridge = ConfigBridge(default_similarity=similarity)
            gui_config = bridge.build_config(
                name=name,
                states=ui_states,
                transitions=ui_transitions,
                state_images=state_images,
                description=description,
            )

            print(
                f"[info    ] EXECUTOR: Multi-state capture complete: "
                f"{len(ui_states)} states, {len(ui_transitions)} transitions",
                file=sys.stderr,
                flush=True,
            )

            return {
                "success": True,
                "config": gui_config,
                "stats": {
                    "state_count": len(ui_states),
                    "transition_count": len(ui_transitions),
                    "image_count": len(gui_config.get("images", [])),
                    "total_elements_seen": len(seen_ids),
                    "interactions_processed": len(interactions),
                },
            }

        except Exception as e:
            print(
                f"[error   ] EXECUTOR: gui_config_capture_multi_state failed: {e}",
                file=sys.stderr,
                flush=True,
            )
            import traceback

            traceback.print_exc(file=sys.stderr)
            return {"success": False, "error": str(e)}

    def _perform_ui_bridge_action(
        self, api_port: int, action_type: str, target: str
    ) -> None:
        """Perform a UI action via the runner's UI Bridge HTTP API.

        Args:
            api_port: Runner API port
            action_type: Action to perform (click, scroll, etc.)
            target: Element ID to act on
        """
        import http.client
        import json as _json
        import sys

        try:
            conn = http.client.HTTPConnection("127.0.0.1", api_port, timeout=10)
            conn.request(
                "POST",
                f"/ui-bridge/control/element/{target}/action",
                _json.dumps({"action": action_type}).encode(),
                {"Content-Type": "application/json"},
            )
            resp = conn.getresponse()
            resp.read()
            conn.close()
            print(
                f"[info    ] EXECUTOR: UI action {action_type} on {target}: {resp.status}",
                file=sys.stderr,
                flush=True,
            )
        except Exception as e:
            print(
                f"[warn    ] EXECUTOR: UI action failed: {action_type} on {target}: {e}",
                file=sys.stderr,
                flush=True,
            )

    def _fetch_ui_bridge_element_captures(
        self,
        api_port: int,
        element_ids: list[str] | None = None,
    ) -> dict[str, dict[str, Any]] | None:
        """Capture element images via the UI Bridge (DOM-based, no screen capture).

        Uses html2canvas in the frontend to render each element directly from
        the DOM. This produces correct images regardless of window z-order.

        Args:
            api_port: Runner API port
            element_ids: Optional list of element IDs to capture.
                If None, captures all visible elements.

        Returns:
            Dict mapping element_id to {base64_png, width, height}, or None on failure.
        """
        import http.client
        import json as _json
        import sys

        try:
            body = {}
            if element_ids is not None:
                body["element_ids"] = element_ids

            conn = http.client.HTTPConnection("127.0.0.1", api_port, timeout=30)
            conn.request(
                "POST",
                "/ui-bridge/control/capture-element-images",
                _json.dumps(body).encode(),
                {"Content-Type": "application/json"},
            )
            resp = conn.getresponse()
            raw = resp.read().decode("utf-8")
            conn.close()

            if resp.status != 200:
                print(
                    f"[warn    ] EXECUTOR: UI Bridge capture returned status {resp.status}",
                    file=sys.stderr,
                    flush=True,
                )
                return None

            response = _json.loads(raw)
            data = response.get("data", response)
            captures = data.get("captures", {})
            count = len(captures)
            print(
                f"[info    ] EXECUTOR: Got {count} element captures from UI Bridge",
                file=sys.stderr,
                flush=True,
            )
            return captures

        except Exception as e:
            print(
                f"[warn    ] EXECUTOR: Failed to capture element images via UI Bridge: {e}",
                file=sys.stderr,
                flush=True,
            )
            return None

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

    # =========================================================================
    # Integration Testing Handlers
    # =========================================================================

    def _get_integration_testing_service(self) -> IntegrationTestingService:
        """Get or create the integration testing service."""
        if self._integration_testing_service is None:
            self._integration_testing_service = IntegrationTestingService(
                emit_log_fn=self.event_manager.emit_log,
                emit_event_fn=self.event_manager.emit_event,
            )
        return self._integration_testing_service  # type: ignore[no-any-return]

    def _handle_testing_get_states(self) -> dict[str, Any]:
        """Handle get states command for integration testing."""
        service = self._get_integration_testing_service()
        states = service.get_states()
        return {"success": True, "states": states}

    def _handle_testing_get_transitions(self) -> dict[str, Any]:
        """Handle get transitions command for integration testing."""
        service = self._get_integration_testing_service()
        transitions = service.get_transitions()
        return {"success": True, "transitions": transitions}

    def _handle_testing_find_path(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle find path command for integration testing."""
        service = self._get_integration_testing_service()
        from_state = params.get("from_state", "")
        to_state = params.get("to_state", "")

        if not from_state or not to_state:
            return {"success": False, "error": "from_state and to_state are required"}

        result = service.find_path(from_state, to_state)
        return result

    def _handle_testing_traverse_to_state(
        self, params: dict[str, Any]
    ) -> dict[str, Any]:
        """Handle traverse to state command for integration testing."""
        service = self._get_integration_testing_service()
        target_state = params.get("target_state", "")
        execute = params.get("execute", True)

        if not target_state:
            return {"success": False, "error": "target_state is required"}

        result = service.traverse_to_state(target_state, execute=execute)
        return result

    def _handle_testing_get_active_states(self) -> dict[str, Any]:
        """Handle get active states command for integration testing."""
        service = self._get_integration_testing_service()
        active_states = service.get_active_states()
        return {"success": True, "active_states": active_states}

    def _handle_testing_set_mock_mode(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle set mock mode command for integration testing."""
        service = self._get_integration_testing_service()
        mode = params.get("mode", "disabled")
        return service.set_mock_mode(mode)

    def _handle_testing_mock_click(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle mock click command for integration testing."""
        service = self._get_integration_testing_service()
        x = params.get("x", 0)
        y = params.get("y", 0)
        button = params.get("button", "left")
        clicks = params.get("clicks", 1)
        return service.mock_click(x, y, button, clicks)

    def _handle_testing_mock_type(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle mock type command for integration testing."""
        service = self._get_integration_testing_service()
        text = params.get("text", "")
        delay_ms = params.get("delay_ms", 50)
        return service.mock_type(text, delay_ms)

    def _handle_testing_mock_screenshot(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle mock screenshot command for integration testing."""
        service = self._get_integration_testing_service()
        monitor_index = params.get("monitor_index")
        return service.mock_screenshot(monitor_index)

    def _handle_testing_get_mocked_actions(self) -> dict[str, Any]:
        """Handle get mocked actions command for integration testing."""
        service = self._get_integration_testing_service()
        actions = service.get_mocked_actions()
        return {"success": True, "actions": actions}

    def _handle_testing_clear_mocked_actions(self) -> dict[str, Any]:
        """Handle clear mocked actions command for integration testing."""
        service = self._get_integration_testing_service()
        return service.clear_mocked_actions()

    def _handle_testing_start_run(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle start test run command for integration testing."""
        service = self._get_integration_testing_service()
        name = params.get("name", "Test Run")
        config_path = params.get("config_path")
        metadata = params.get("metadata")
        return service.start_test_run(name, config_path, metadata)

    def _handle_testing_run_assertion(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle run assertion command for integration testing."""
        service = self._get_integration_testing_service()
        assertion_type = params.get("assertion_type", "")
        target = params.get("target", "")
        expected = params.get("expected")
        timeout_seconds = params.get("timeout_seconds", 30.0)

        if not assertion_type or not target:
            return {"success": False, "error": "assertion_type and target are required"}

        return service.run_assertion(assertion_type, target, expected, timeout_seconds)

    def _handle_testing_run_test_case(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle run test case command for integration testing."""
        service = self._get_integration_testing_service()
        test_case = params.get("test_case", {})
        return service.run_test_case(test_case)

    def _handle_testing_end_run(self) -> dict[str, Any]:
        """Handle end test run command for integration testing."""
        service = self._get_integration_testing_service()
        return service.end_test_run()

    def _handle_testing_get_run(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle get test run command for integration testing."""
        service = self._get_integration_testing_service()
        run_id = params.get("run_id", "")
        if not run_id:
            return {"success": False, "error": "run_id is required"}
        return service.get_test_run(run_id)

    def _handle_testing_get_status(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle get test status command for integration testing."""
        service = self._get_integration_testing_service()
        run_id = params.get("run_id", "")
        if not run_id:
            return {"success": False, "error": "run_id is required"}
        return service.get_test_status(run_id)

    def _handle_testing_get_results(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle get test results command for integration testing."""
        service = self._get_integration_testing_service()
        run_id = params.get("run_id", "")
        if not run_id:
            return {"success": False, "error": "run_id is required"}
        return service.get_test_results(run_id)

    def _handle_testing_list_runs(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle list test runs command for integration testing."""
        service = self._get_integration_testing_service()
        limit = params.get("limit", 50)
        runs = service.list_test_runs(limit)
        return {"success": True, "runs": runs}

    # =========================================================================
    # Model Management Handlers
    # =========================================================================

    def _handle_models_list(self) -> dict[str, Any]:
        """List all available models with their download status.

        Returns:
            Dictionary with:
                - success: True
                - models: List of model info dictionaries with:
                    - id: Model identifier
                    - name: Human-readable name
                    - type: Model type (sam3, clip, easyocr)
                    - description: Model description
                    - size_bytes: Model size in bytes
                    - available: Whether model is downloaded
        """
        try:
            manager = get_model_manager()
            models = manager.list_models()
            return {"success": True, "models": models}
        except Exception as e:
            logger.exception(f"Failed to list models: {e}")
            return {"success": False, "error": str(e)}

    def _handle_models_download(self, params: dict[str, Any]) -> dict[str, Any]:
        """Download a model.

        Note: This is a synchronous download. For large models, progress
        events are emitted during download.

        Args:
            params:
                - model_id: Model identifier (e.g., "sam3", "clip_vit_b32")
                - force: Re-download even if already available (default: False)

        Returns:
            Dictionary with:
                - success: Whether download succeeded
                - path: Path to the downloaded model
                - error: Error message (if failed)
        """
        try:
            model_id = params.get("model_id", "")
            force = params.get("force", False)

            if not model_id:
                return {"success": False, "error": "model_id is required"}

            manager = get_model_manager()

            # Progress callback that emits events (use emit_event_wrapper for string event types)
            def on_progress(progress: int) -> None:
                self.event_manager.emit_event_wrapper(
                    "model_download_progress",
                    {
                        "model_id": model_id,
                        "progress": progress,
                    },
                )

            path = manager.download(
                model_id, progress_callback=on_progress, force=force
            )

            return {
                "success": True,
                "path": str(path),
                "model_id": model_id,
            }

        except Exception as e:
            logger.exception(f"Failed to download model {params.get('model_id')}: {e}")
            return {"success": False, "error": str(e)}

    def _handle_models_delete(self, params: dict[str, Any]) -> dict[str, Any]:
        """Delete a downloaded model.

        Args:
            params:
                - model_id: Model identifier

        Returns:
            Dictionary with:
                - success: Whether deletion succeeded
                - deleted: True if model was deleted
                - error: Error message (if failed)
        """
        try:
            model_id = params.get("model_id", "")

            if not model_id:
                return {"success": False, "error": "model_id is required"}

            manager = get_model_manager()
            deleted = manager.delete(model_id)

            return {
                "success": True,
                "deleted": deleted,
                "model_id": model_id,
            }

        except Exception as e:
            logger.exception(f"Failed to delete model {params.get('model_id')}: {e}")
            return {"success": False, "error": str(e)}

    def _handle_models_status(self, params: dict[str, Any]) -> dict[str, Any]:
        """Get status of a specific model.

        Args:
            params:
                - model_id: Model identifier

        Returns:
            Dictionary with:
                - success: True
                - model_id: Model identifier
                - available: Whether model is downloaded
                - path: Path to model (if available)
                - info: Model info (name, type, size, etc.)
        """
        try:
            model_id = params.get("model_id", "")

            if not model_id:
                return {"success": False, "error": "model_id is required"}

            manager = get_model_manager()
            available = manager.is_available(model_id)
            path = manager.get_model_path(model_id)
            info = manager.get_model_info(model_id)

            return {
                "success": True,
                "model_id": model_id,
                "available": available,
                "path": str(path) if path else None,
                "info": (
                    {
                        "name": info.name,
                        "type": info.model_type.value,
                        "description": info.description,
                        "size_bytes": info.size_bytes,
                    }
                    if info
                    else None
                ),
            }

        except Exception as e:
            logger.exception(
                f"Failed to get model status {params.get('model_id')}: {e}"
            )
            return {"success": False, "error": str(e)}

    def _handle_models_disk_usage(self) -> dict[str, Any]:
        """Get disk usage for all downloaded models.

        Returns:
            Dictionary with:
                - success: True
                - total_bytes: Total disk usage in bytes
                - models: Dictionary of model_id -> size in bytes
                - models_dir: Path to models directory
        """
        try:
            manager = get_model_manager()
            usage = manager.get_disk_usage()

            return {
                "success": True,
                **usage,
            }

        except Exception as e:
            logger.exception(f"Failed to get disk usage: {e}")
            return {"success": False, "error": str(e)}

    # ============================================================================
    # Accessibility Capture Handlers
    # ============================================================================

    def _handle_capture_accessibility(self, params: dict[str, Any]) -> dict[str, Any]:
        """Capture accessibility tree from a browser via CDP.

        Args:
            params: Command parameters:
                - target: Target to capture ("auto", URL, or page index)
                - cdp_host: CDP host (default: localhost)
                - cdp_port: CDP port (default: 9222)
                - interactive_only: Only include interactive elements
                - include_hidden: Include hidden elements
                - max_depth: Maximum tree depth

        Returns:
            Dictionary with:
                - success: Whether capture succeeded
                - snapshot: Serialized AccessibilitySnapshot
                - ai_context: AI-friendly text representation
                - stats: Capture statistics
                - error: Error message if failed
        """
        import asyncio

        try:
            service = self._get_accessibility_capture_service()

            target = params.get("target", "auto")
            cdp_host = params.get("cdp_host", "localhost")
            cdp_port = params.get("cdp_port", 9222)
            interactive_only = params.get("interactive_only", False)
            include_hidden = params.get("include_hidden", False)
            max_depth = params.get("max_depth")

            loop = self._get_or_create_async_loop()
            future = asyncio.run_coroutine_threadsafe(
                service.capture_accessibility_tree(
                    target=target,
                    cdp_host=cdp_host,
                    cdp_port=cdp_port,
                    interactive_only=interactive_only,
                    include_hidden=include_hidden,
                    max_depth=max_depth,
                ),
                loop,
            )
            result = future.result(timeout=60)

            self.event_manager.emit_log(
                "info" if result.get("success") else "error",
                f"Accessibility capture: {'success' if result.get('success') else result.get('error', 'failed')}",
            )

            return result

        except Exception as e:
            logger.exception(f"Failed to capture accessibility tree: {e}")
            return {"success": False, "error": str(e)}

    def _handle_click_ref(self, params: dict[str, Any]) -> dict[str, Any]:
        """Click an element by its accessibility ref.

        Args:
            params: Command parameters:
                - ref: Reference ID (e.g., "@e3")

        Returns:
            Dictionary with success status and element info
        """
        import asyncio

        try:
            service = self._get_accessibility_capture_service()
            ref = params.get("ref")

            if not ref:
                return {"success": False, "error": "ref parameter is required"}

            loop = self._get_or_create_async_loop()
            future = asyncio.run_coroutine_threadsafe(service.click_ref(ref), loop)
            result = future.result(timeout=30)

            self.event_manager.emit_log(
                "info" if result.get("success") else "warning",
                f"Click ref {ref}: {'success' if result.get('success') else result.get('error', 'failed')}",
            )

            return result

        except Exception as e:
            logger.exception(f"Failed to click ref: {e}")
            return {"success": False, "error": str(e)}

    def _handle_fill_ref(self, params: dict[str, Any]) -> dict[str, Any]:
        """Type text into an element by its accessibility ref.

        Args:
            params: Command parameters:
                - ref: Reference ID (e.g., "@e2")
                - value: Text to type
                - clear_first: Clear existing content first

        Returns:
            Dictionary with success status and element info
        """
        import asyncio

        try:
            service = self._get_accessibility_capture_service()
            ref = params.get("ref")
            value = params.get("value", "")
            clear_first = params.get("clear_first", False)

            if not ref:
                return {"success": False, "error": "ref parameter is required"}

            loop = self._get_or_create_async_loop()
            future = asyncio.run_coroutine_threadsafe(
                service.fill_ref(ref, value, clear_first=clear_first), loop
            )
            result = future.result(timeout=30)

            self.event_manager.emit_log(
                "info" if result.get("success") else "warning",
                f"Fill ref {ref}: {'success' if result.get('success') else result.get('error', 'failed')}",
            )

            return result

        except Exception as e:
            logger.exception(f"Failed to fill ref: {e}")
            return {"success": False, "error": str(e)}

    def _handle_focus_ref(self, params: dict[str, Any]) -> dict[str, Any]:
        """Focus an element by its accessibility ref.

        Args:
            params: Command parameters:
                - ref: Reference ID (e.g., "@e5")

        Returns:
            Dictionary with success status and element info
        """
        import asyncio

        try:
            service = self._get_accessibility_capture_service()
            ref = params.get("ref")

            if not ref:
                return {"success": False, "error": "ref parameter is required"}

            loop = self._get_or_create_async_loop()
            future = asyncio.run_coroutine_threadsafe(service.focus_ref(ref), loop)
            result = future.result(timeout=30)

            self.event_manager.emit_log(
                "info" if result.get("success") else "warning",
                f"Focus ref {ref}: {'success' if result.get('success') else result.get('error', 'failed')}",
            )

            return result

        except Exception as e:
            logger.exception(f"Failed to focus ref: {e}")
            return {"success": False, "error": str(e)}

    def _handle_get_ref(self, params: dict[str, Any]) -> dict[str, Any]:
        """Get element details by its accessibility ref.

        Args:
            params: Command parameters:
                - ref: Reference ID (e.g., "@e1")

        Returns:
            Dictionary with element details or error
        """
        try:
            service = self._get_accessibility_capture_service()
            ref = params.get("ref")

            if not ref:
                return {"success": False, "error": "ref parameter is required"}

            return service.get_element_by_ref(ref)

        except Exception as e:
            logger.exception(f"Failed to get ref: {e}")
            return {"success": False, "error": str(e)}

    def _handle_get_accessibility_snapshot(self) -> dict[str, Any]:
        """Get the current cached accessibility snapshot.

        Returns:
            Dictionary with snapshot data or error
        """
        try:
            service = self._get_accessibility_capture_service()
            return service.get_current_snapshot()

        except Exception as e:
            logger.exception(f"Failed to get accessibility snapshot: {e}")
            return {"success": False, "error": str(e)}

    def _handle_get_accessibility_ai_context(
        self, params: dict[str, Any]
    ) -> dict[str, Any]:
        """Get AI-friendly context from the current accessibility snapshot.

        Args:
            params: Command parameters:
                - max_elements: Maximum number of elements to include
                - interactive_only: Only include interactive elements

        Returns:
            Dictionary with AI context string
        """
        try:
            service = self._get_accessibility_capture_service()
            max_elements = params.get("max_elements", 100)
            interactive_only = params.get("interactive_only", True)

            return service.get_ai_context(
                max_elements=max_elements, interactive_only=interactive_only
            )

        except Exception as e:
            logger.exception(f"Failed to get accessibility AI context: {e}")
            return {"success": False, "error": str(e)}

    def _handle_find_accessibility_elements(
        self, params: dict[str, Any]
    ) -> dict[str, Any]:
        """Find elements matching criteria in the accessibility tree.

        Args:
            params: Command parameters:
                - role: Role(s) to match
                - name: Exact name to match
                - name_contains: Partial name match
                - is_interactive: Filter by interactivity

        Returns:
            Dictionary with matching elements
        """
        import asyncio

        try:
            service = self._get_accessibility_capture_service()

            role = params.get("role")
            name = params.get("name")
            name_contains = params.get("name_contains")
            is_interactive = params.get("is_interactive")

            loop = self._get_or_create_async_loop()
            future = asyncio.run_coroutine_threadsafe(
                service.find_elements(
                    role=role,
                    name=name,
                    name_contains=name_contains,
                    is_interactive=is_interactive,
                ),
                loop,
            )
            result = future.result(timeout=30)

            return result

        except Exception as e:
            logger.exception(f"Failed to find accessibility elements: {e}")
            return {"success": False, "error": str(e)}

    def _handle_disconnect_accessibility(self) -> dict[str, Any]:
        """Disconnect from the accessibility source.

        Returns:
            Dictionary with disconnect status
        """
        import asyncio

        try:
            service = self._get_accessibility_capture_service()

            loop = self._get_or_create_async_loop()
            future = asyncio.run_coroutine_threadsafe(service.disconnect(), loop)
            result = future.result(timeout=10)

            self.event_manager.emit_log("info", "Accessibility capture disconnected")

            return result

        except Exception as e:
            logger.exception(f"Failed to disconnect accessibility: {e}")
            return {"success": False, "error": str(e)}

    def _handle_scan_cdp_ports(self, params: dict[str, Any]) -> dict[str, Any]:
        """Scan common CDP ports to find available browsers.

        Args:
            params: Command parameters:
                - host: Host to scan (default: localhost)
                - ports: List of ports to scan (default: common ports)
                - timeout: Timeout per port in seconds

        Returns:
            Dictionary with available ports and targets
        """
        import asyncio

        try:
            service = self._get_accessibility_capture_service()

            host = params.get("host", "localhost")
            ports = params.get("ports")
            timeout = params.get("timeout", 2.0)

            loop = self._get_or_create_async_loop()
            future = asyncio.run_coroutine_threadsafe(
                service.scan_cdp_ports(host, ports, timeout),
                loop,
            )
            result = future.result(timeout=30)

            self.event_manager.emit_log(
                "info",
                f"CDP port scan complete: {len(result.get('available_ports', []))} ports found",
            )

            return result

        except Exception as e:
            logger.exception(f"Failed to scan CDP ports: {e}")
            return {"success": False, "error": str(e)}

    def _handle_list_browser_targets(self, params: dict[str, Any]) -> dict[str, Any]:
        """List available browser page targets on a CDP port.

        Args:
            params: Command parameters:
                - host: CDP host (default: localhost)
                - port: CDP port (default: 9222)

        Returns:
            Dictionary with list of page targets
        """
        import asyncio

        try:
            service = self._get_accessibility_capture_service()

            host = params.get("host", "localhost")
            port = params.get("port", 9222)

            loop = self._get_or_create_async_loop()
            future = asyncio.run_coroutine_threadsafe(
                service.list_browser_targets(host, port),
                loop,
            )
            result = future.result(timeout=10)

            self.event_manager.emit_log(
                "info",
                f"Listed {result.get('count', 0)} browser targets on port {port}",
            )

            return result

        except Exception as e:
            logger.exception(f"Failed to list browser targets: {e}")
            return {"success": False, "error": str(e)}

    def _handle_auto_connect_accessibility(
        self, params: dict[str, Any]
    ) -> dict[str, Any]:
        """Automatically connect to the first available CDP target.

        Args:
            params: Command parameters:
                - host: Host to scan (default: localhost)
                - preferred_ports: List of ports to try in order

        Returns:
            Dictionary with connection result and captured snapshot
        """
        import asyncio

        try:
            service = self._get_accessibility_capture_service()

            host = params.get("host", "localhost")
            preferred_ports = params.get("preferred_ports")

            loop = self._get_or_create_async_loop()
            future = asyncio.run_coroutine_threadsafe(
                service.auto_connect(host, preferred_ports),
                loop,
            )
            result = future.result(timeout=60)

            if result.get("success"):
                target = result.get("target", {})
                self.event_manager.emit_log(
                    "info",
                    f"Auto-connected to CDP on port {result.get('port')}: {target.get('title', 'Unknown')}",
                )
            else:
                self.event_manager.emit_log(
                    "warning",
                    f"Auto-connect failed: {result.get('error')}",
                )

            return result

        except Exception as e:
            logger.exception(f"Failed to auto-connect accessibility: {e}")
            return {"success": False, "error": str(e)}

    # ============================================================================
    # AWAS (AI Web Action Standard) Handlers
    # ============================================================================

    def _get_awas_discovery_service(self):
        """Get or create the AWAS discovery service instance."""
        if not hasattr(self, "_awas_discovery_service"):
            try:
                from qontinui.awas import AwasDiscoveryService

                self._awas_discovery_service = AwasDiscoveryService()
            except ImportError:
                self._awas_discovery_service = None
        return self._awas_discovery_service

    def _get_awas_executor(self):
        """Get or create the AWAS executor instance."""
        if not hasattr(self, "_awas_executor"):
            try:
                from qontinui.awas import AwasExecutor

                self._awas_executor = AwasExecutor()
            except ImportError:
                self._awas_executor = None
        return self._awas_executor

    def _handle_awas_discover(self, params: dict[str, Any]) -> dict[str, Any]:
        """Discover AWAS manifest for a website.

        Args:
            params: Command parameters:
                - url: Base URL of the website to discover (required)
                - force_refresh: Whether to bypass cache (default: False)

        Returns:
            Dictionary with:
                - success: Whether discovery succeeded
                - manifest: Parsed AWAS manifest data (if success)
                - error: Error message (if failed)
        """
        import asyncio

        try:
            service = self._get_awas_discovery_service()
            if not service:
                return {"success": False, "error": "AWAS module not available"}

            url = params.get("url")
            if not url:
                return {"success": False, "error": "url parameter is required"}

            force_refresh = params.get("force_refresh", False)

            # Clear cache if force refresh
            if force_refresh:
                service.clear_cache(url)

            loop = self._get_or_create_async_loop()
            future = asyncio.run_coroutine_threadsafe(
                service.discover(url),
                loop,
            )
            manifest = future.result(timeout=30)

            if manifest is None:
                return {
                    "success": False,
                    "error": f"No AWAS manifest found at {url}",
                }

            # Convert manifest to dict for JSON serialization
            manifest_data = manifest.model_dump(by_alias=True, exclude_none=True)

            self.event_manager.emit_log(
                "info",
                f"Discovered AWAS manifest for {manifest.app_name} with {len(manifest.actions)} actions",
            )

            return {
                "success": True,
                "manifest": manifest_data,
                "app_name": manifest.app_name,
                "action_count": len(manifest.actions),
                "conformance_level": manifest.conformance_level.value,
            }

        except Exception as e:
            logger.exception(f"Failed to discover AWAS manifest: {e}")
            return {"success": False, "error": str(e)}

    def _handle_awas_execute(self, params: dict[str, Any]) -> dict[str, Any]:
        """Execute an AWAS action.

        Args:
            params: Command parameters:
                - url: Base URL of the website (required)
                - action_id: ID of the action to execute (required)
                - action_params: Parameters for the action (optional)
                - credentials: Authentication credentials (optional)
                - timeout_seconds: Request timeout (optional)

        Returns:
            Dictionary with:
                - success: Whether execution succeeded
                - status_code: HTTP status code
                - response_body: Response data
                - response_time_ms: Response time in milliseconds
                - error: Error message (if failed)
        """
        import asyncio

        try:
            discovery_service = self._get_awas_discovery_service()
            executor = self._get_awas_executor()

            if not discovery_service or not executor:
                return {"success": False, "error": "AWAS module not available"}

            url = params.get("url")
            action_id = params.get("action_id")
            action_params = params.get("action_params", {})
            credentials = params.get("credentials", {})
            timeout_seconds = params.get("timeout_seconds")

            if not url:
                return {"success": False, "error": "url parameter is required"}
            if not action_id:
                return {"success": False, "error": "action_id parameter is required"}

            # First discover the manifest
            loop = self._get_or_create_async_loop()
            future = asyncio.run_coroutine_threadsafe(
                discovery_service.discover(url),
                loop,
            )
            manifest = future.result(timeout=30)

            if manifest is None:
                return {"success": False, "error": f"No AWAS manifest found at {url}"}

            # Execute the action
            execute_kwargs = {
                "manifest": manifest,
                "action_id": action_id,
                "params": action_params,
                "credentials": credentials,
            }
            if timeout_seconds is not None:
                execute_kwargs["timeout_seconds"] = timeout_seconds

            future = asyncio.run_coroutine_threadsafe(
                executor.execute(**execute_kwargs),
                loop,
            )
            result = future.result(timeout=timeout_seconds or 60)

            self.event_manager.emit_log(
                "info" if result.success else "warning",
                f"AWAS action '{action_id}' {'succeeded' if result.success else 'failed'}: "
                f"status={result.status_code}, time={result.response_time_ms}ms",
            )

            return {
                "success": result.success,
                "action_id": result.action_id,
                "status_code": result.status_code,
                "response_body": result.response_body,
                "response_time_ms": result.response_time_ms,
                "error": result.error,
            }

        except Exception as e:
            logger.exception(f"Failed to execute AWAS action: {e}")
            return {"success": False, "error": str(e)}

    def _handle_awas_check_support(self, params: dict[str, Any]) -> dict[str, Any]:
        """Check if a website supports AWAS.

        Args:
            params: Command parameters:
                - url: Base URL of the website (required)

        Returns:
            Dictionary with:
                - success: Whether check completed
                - supported: Whether AWAS is supported
                - app_name: Application name (if supported)
                - action_count: Number of available actions
                - conformance_level: AWAS conformance level
        """
        import asyncio

        try:
            service = self._get_awas_discovery_service()
            if not service:
                return {"success": False, "error": "AWAS module not available"}

            url = params.get("url")
            if not url:
                return {"success": False, "error": "url parameter is required"}

            loop = self._get_or_create_async_loop()
            future = asyncio.run_coroutine_threadsafe(
                service.check_awas_support(url),
                loop,
            )
            result = future.result(timeout=30)

            self.event_manager.emit_log(
                "info",
                f"AWAS support check for {url}: {'supported' if result.get('supported') else 'not supported'}",
            )

            return {"success": True, **result}

        except Exception as e:
            logger.exception(f"Failed to check AWAS support: {e}")
            return {"success": False, "error": str(e)}

    def _handle_awas_list_actions(self, params: dict[str, Any]) -> dict[str, Any]:
        """List available AWAS actions for a website.

        Args:
            params: Command parameters:
                - url: Base URL of the website (required)
                - read_only_only: Only return read-only actions (default: False)

        Returns:
            Dictionary with:
                - success: Whether listing succeeded
                - actions: List of action summaries
                - app_name: Application name
        """
        import asyncio

        try:
            service = self._get_awas_discovery_service()
            if not service:
                return {"success": False, "error": "AWAS module not available"}

            url = params.get("url")
            if not url:
                return {"success": False, "error": "url parameter is required"}

            read_only_only = params.get("read_only_only", False)

            loop = self._get_or_create_async_loop()
            future = asyncio.run_coroutine_threadsafe(
                service.discover(url),
                loop,
            )
            manifest = future.result(timeout=30)

            if manifest is None:
                return {"success": False, "error": f"No AWAS manifest found at {url}"}

            # Get actions (filtered if requested)
            actions = (
                manifest.get_read_only_actions() if read_only_only else manifest.actions
            )

            # Build action summaries
            action_summaries = []
            for action in actions:
                summary = {
                    "id": action.id,
                    "name": action.name,
                    "method": action.method.value,
                    "endpoint": action.endpoint,
                    "intent": action.intent,
                    "side_effect": action.side_effect,
                    "is_read_only": action.is_read_only,
                    "parameter_count": len(action.parameters),
                }
                action_summaries.append(summary)

            self.event_manager.emit_log(
                "info",
                f"Listed {len(action_summaries)} AWAS actions for {manifest.app_name}",
            )

            return {
                "success": True,
                "app_name": manifest.app_name,
                "actions": action_summaries,
            }

        except Exception as e:
            logger.exception(f"Failed to list AWAS actions: {e}")
            return {"success": False, "error": str(e)}

    def _handle_awas_extract_elements(self, params: dict[str, Any]) -> dict[str, Any]:
        """Extract AWAS elements from HTML content.

        Args:
            params: Command parameters:
                - html: HTML content to parse (required)
                - page_url: URL of the page (optional, for context)

        Returns:
            Dictionary with:
                - success: Whether extraction succeeded
                - elements: List of extracted AWAS elements
        """
        try:
            service = self._get_awas_discovery_service()
            if not service:
                return {"success": False, "error": "AWAS module not available"}

            html = params.get("html")
            if not html:
                return {"success": False, "error": "html parameter is required"}

            page_url = params.get("page_url")

            elements = service.extract_elements(html, page_url)

            # Convert elements to dicts for JSON serialization
            element_data = []
            for elem in elements:
                element_data.append(elem.model_dump(exclude_none=True))

            self.event_manager.emit_log(
                "info",
                f"Extracted {len(elements)} AWAS elements from HTML",
            )

            return {
                "success": True,
                "elements": element_data,
                "element_count": len(elements),
            }

        except Exception as e:
            logger.exception(f"Failed to extract AWAS elements: {e}")
            return {"success": False, "error": str(e)}


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
                result = executor.handle_command(command)
                # Wrap response data in the format expected by Rust:
                # { type: "response", id: "...", success: bool, data: {...}, error: Option<String> }
                #
                # The result from handlers can be in two formats:
                # 1. {"success": True, "data": {...}, "error": ""}  - from AI generator services
                # 2. {"success": True, "some_field": ..., "error": None}  - from other handlers
                #
                # For format 1, we extract the nested "data" field directly.
                # For format 2, we build a data dict from non-success/error fields.
                if "data" in result and isinstance(result.get("data"), dict):
                    # AI generator format - extract the nested data directly
                    response_data = result.get("data")
                else:
                    # Legacy format - build data from remaining fields
                    response_data = {
                        k: v for k, v in result.items() if k not in ("success", "error")
                    }

                response = {
                    "type": "response",
                    "id": command.get("id"),
                    "success": result.get("success", False),
                    "data": response_data,
                    "error": result.get("error"),
                }

                # Debug log for AI generation responses
                if cmd_name in (
                    "generate_test_and_agentic_step",
                    "generate_context_with_ai",
                    "generate_api_request_with_ai",
                    "generate_task_prompt_with_ai",
                ):
                    logger.debug(
                        f"[RESPONSE] {cmd_name}: success={response['success']}"
                    )
                    if response_data:
                        logger.debug(
                            f"[RESPONSE] data keys: {list(response_data.keys()) if isinstance(response_data, dict) else type(response_data)}"
                        )

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
