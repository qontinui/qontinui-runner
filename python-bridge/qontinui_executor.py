#!/usr/bin/env python3
"""
Qontinui Executor - Main Entry Point

This module composes specialized modules following the Single Responsibility Principle:
- event_manager.py: Event handling and emission
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
from typing import Any, ClassVar

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

# TestResultsHandler module may not exist; import gracefully
try:
    from test_results_handler import TestResultsHandler  # noqa: E402
except ImportError:
    TestResultsHandler = None  # type: ignore[assignment, misc]

# UI Bridge explorer - lightweight, doesn't need ML stack
from services.ui_bridge_explorer_service import UIBridgeExplorerService  # noqa: E402

# isort: split
# The per-domain command mixins. Importing the package runs executor_commands._shared,
# which owns the services that may need the heavy ML stack (imported all-or-nothing,
# with None placeholders) and the qontinui availability check, at this same point in
# the import order as when both blocks lived here.
from executor_commands import (  # noqa: E402
    AccessibilityCommands,
    AiGenerateCommands,
    AwasCommands,
    CaptureCommands,
    ClickCaptureCommands,
    CommandEntry,
    ExtractionCommands,
    GuiConfigCommands,
    ModelCommands,
    RecordingCommands,
    StateMachineCommands,
    TestingCommands,
    UiBridgeDiscoveryCommands,
    build_command_table,
)
from executor_commands._shared import (  # noqa: E402
    QONTINUI_AVAILABLE,
    QONTINUI_IMPORT_ERROR,
    GUIAutomation,
    InputMonitorService,
    ScreenshotService,
    UITarsExtractionService,
    UnifiedDataCollector,
    get_settings,
    navigation_api,
)

if not QONTINUI_AVAILABLE:
    import_error_details, full_traceback = QONTINUI_IMPORT_ERROR
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


class QontinuiExecutor(
    TestingCommands,
    StateMachineCommands,
    ExtractionCommands,
    CaptureCommands,
    UiBridgeDiscoveryCommands,
    AccessibilityCommands,
    AiGenerateCommands,
    ClickCaptureCommands,
    GuiConfigCommands,
    RecordingCommands,
    ModelCommands,
    AwasCommands,
):
    """
    Main executor that composes specialized modules.

    Responsibilities:
    - Coordinate all specialized modules
    - Handle commands from Rust bridge
    - Maintain execution state
    - Provide workflow execution interface
    """

    # Command name -> handler method. ``build_command_table`` (executor_commands)
    # validates every entry at import and decides once whether each method takes
    # ``params``. Adding a command is one entry here (or in a mixin's COMMANDS).
    COMMANDS: ClassVar[dict[str, str]] = {
        "load": "_cmd_load",
        "start": "_cmd_start",
        "stop": "_cmd_stop",
        "pause": "_cmd_pause",
        "resume": "_cmd_resume",
        "execute_action": "_cmd_execute_action",
        "status": "_cmd_status",
        "set_debug_settings": "_cmd_set_debug_settings",
        "update_capture_settings": "_cmd_update_capture_settings",
        "manual_capture_status": "_cmd_manual_capture_status",
        "set_input_capture_enabled": "_cmd_set_input_capture_enabled",
        "get_input_validation_status": "_cmd_get_input_validation_status",
        # Test Results Handler commands (for QA Dashboard)
        "test_results_configure": "_cmd_test_results_configure",
        "test_results_status": "_cmd_test_results_status",
        "ping": "_cmd_ping",
        "navigate_to_state": "_cmd_navigate_to_state",
        # Remote workflow execution from web app
        "execute_workflow": "_handle_execute_workflow",
    }

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

        # Initialize TestResultsHandler for QA dashboard submission
        if TestResultsHandler is not None:
            self.test_results_handler = TestResultsHandler(emit_log_fn=self.event_manager.emit_log)
        else:
            self.test_results_handler = None

        # Initialize CaptureManager
        self.capture_manager = CaptureManager(
            emit_log_fn=self.event_manager.emit_log,
            emit_event_fn=self.event_manager.emit_event,
        )

        # Initialize TrainingExportCoordinator
        self.training_export = TrainingExportCoordinator(emit_log_fn=self.event_manager.emit_log)

        # Trajectory logger (initialised later if QONTINUI_TRAJECTORY_LOGGER=on)
        self._trajectory_logger = None

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
            logger.debug(f"[INIT] Event registry has_listeners: {event_registry.has_listeners}")
            logger.debug("[INIT] EventTranslator initialized and callbacks registered")

        logger.info(f"QontinuiExecutor initialized (library_available={QONTINUI_AVAILABLE})")

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

        from qontinui.state_machine.ui_bridge_runtime import UIBridgeRuntime

        from element_resolver import ElementResolver
        from ui_bridge_http_client import ResolvingUIBridgeClient, UIBridgeHTTPClient

        inner_client = UIBridgeHTTPClient(self._get_runner_api_base())
        resolver = ElementResolver(persistence)
        client = ResolvingUIBridgeClient(inner_client, resolver)

        runtime = UIBridgeRuntime.from_dict(config_data, client)
        self._ui_bridge_runtime = runtime
        self._element_resolver = resolver
        self.event_manager.emit_log("info", "Auto-reloaded persisted state machine")

    def _emit_event_wrapper(self, event_type: str, data: dict[str, Any]):
        """Wrapper for EventTranslator to emit events."""
        self.event_manager.emit_event_wrapper(event_type, data)

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

            # Initialize TrajectoryLogger if opted in
            if os.getenv("QONTINUI_TRAJECTORY_LOGGER", "").lower() == "on":
                try:
                    from services.trajectory_logger import TrajectoryLogger

                    trajectory_output = Path(
                        os.getenv("QONTINUI_EXPORT_DIR", str(run_dir / "dataset"))
                    )
                    max_records = int(os.getenv("QONTINUI_TRAJECTORY_MAX_RECORDS", "500"))

                    # WSM is the preferred success-labelling source for
                    # grounding records. Gated by QONTINUI_WSM_ENABLED
                    # (default "1") so a prod regression can be rolled
                    # back without a code change. When enabled and no
                    # explicit client is provided, TrajectoryLogger
                    # lazy-constructs a canonical WSMClient whose
                    # endpoint is resolved from env vars.
                    wsm_enabled = os.getenv("QONTINUI_WSM_ENABLED", "1").lower() not in {
                        "0",
                        "false",
                        "off",
                        "no",
                    }
                    wsm_client = None
                    # Preserve the legacy override: if the old env var is
                    # set AND the legacy client is importable, honour it
                    # so existing E2E rigs keep working. Otherwise rely
                    # on TrajectoryLogger's lazy construction.
                    try:
                        legacy_endpoint = os.getenv("QONTINUI_WORLD_STATE_VERIFIER_LEGACY_ENDPOINT")
                        if legacy_endpoint:
                            from tests.e2e.broken_accessibility._wsm_client import (
                                WorldStateVerifierClient,
                            )

                            wsm_client = WorldStateVerifierClient(endpoint=legacy_endpoint)
                    except ImportError:
                        pass

                    # Optional OmniParser detector
                    omniparser = None
                    if os.getenv("QONTINUI_OMNIPARSER_ENABLED", "").lower() == "true":
                        try:
                            from qontinui.discovery.element_detection.omniparser_detector import (
                                OmniParserDetector,
                            )

                            omniparser = OmniParserDetector()
                        except ImportError:
                            pass

                    self._trajectory_logger = TrajectoryLogger(
                        output_dir=trajectory_output,
                        max_records_per_session=max_records,
                        wsm_client=wsm_client,
                        omniparser_detector=omniparser,
                        wsm_enabled=wsm_enabled,
                    )
                    self.event_manager.emit_log(
                        "info",
                        f"TrajectoryLogger enabled: {trajectory_output} "
                        f"(max_records={max_records})",
                    )
                except Exception as tl_err:
                    self.event_manager.emit_log(
                        "warning", f"TrajectoryLogger init failed (non-fatal): {tl_err}"
                    )
                    self._trajectory_logger = None

            # Initialize UnifiedDataCollector with combined callback
            training_callback = self.training_export.get_record_callback()

            def combined_record_callback(record):
                """Callback that reports to training export, trajectory logger, and test results."""
                # Call training export callback
                if training_callback:
                    training_callback(record)

                # Call trajectory logger callback
                if self._trajectory_logger:
                    with contextlib.suppress(Exception):
                        self._trajectory_logger.on_record_created(record)

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
                            duration_ms=(int(record.duration_ms) if record.duration_ms else None),
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
            # Set pause event for synchronization
            self.gui_automation.set_pause_event(self._pause_event)
            # Set activity timeline capture callback (screenpipe-inspired)
            self.gui_automation.set_timeline_capture_fn(self.event_manager.emit_timeline_capture)

            # Wire trajectory logger pre-action callback
            if self._trajectory_logger:
                self.gui_automation._pre_action_callback = self._trajectory_logger.on_action_start

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
                self.event_manager.emit_log("info", "Exporting training data on stop...")
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

            self.event_manager.emit_event(EventType.EXECUTION_RESUMED, {"paused": False})

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

    def handle_command(self, command: dict[str, Any]) -> dict[str, Any]:
        """Handle command from Rust bridge."""
        cmd_type = command.get("command")
        # Rust serializes ``params: None`` as JSON null; handlers expect a dict.
        params = command.get("params") or {}

        # Don't log high-frequency commands to avoid flooding the logs
        if cmd_type not in ("ping", "status"):
            self.event_manager.emit_log("info", f"handle_command: received '{cmd_type}'")

        # ``params`` is decoded JSON; handlers that read required keys type it
        # ``dict[str, Any]`` and validate those keys themselves.
        entry = _COMMAND_TABLE.get(cmd_type) if isinstance(cmd_type, str) else None
        if entry is None:
            return {"success": False, "error": f"Unknown command: {cmd_type}"}
        handler = getattr(self, entry.method)
        if entry.takes_params:
            return handler(params)  # type: ignore[no-any-return]
        return handler()  # type: ignore[no-any-return]

    def _cmd_load(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle the ``load`` command."""
        config_path = params.get("config_path")
        if not config_path:
            return {"success": False, "error": "config_path is required"}
        success = self.load_configuration(config_path)
        return {"success": success}

    def _cmd_start(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle the ``start`` command."""
        workflow_id = params.get("workflow_id") or params.get("workflow")
        if not workflow_id:
            return {"success": False, "error": "workflow_id is required"}
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

    def _cmd_stop(self, params: Any) -> dict[str, Any]:
        """Handle the ``stop`` command."""
        self.stop_execution()
        return {"success": True}

    def _cmd_pause(self, params: Any) -> dict[str, Any]:
        """Handle the ``pause`` command."""
        self.pause_execution()
        return {"success": True}

    def _cmd_resume(self, params: Any) -> dict[str, Any]:
        """Handle the ``resume`` command."""
        self.resume_execution()
        return {"success": True}

    def _cmd_execute_action(self, params: Any) -> dict[str, Any]:
        """Handle the ``execute_action`` command."""
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
            success = future.result(timeout=120)  # 2 minute timeout for single actions

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

    def _cmd_status(self, params: Any) -> dict[str, Any]:
        """Handle the ``status`` command."""
        return {
            "success": True,
            "is_running": self.is_running,
            "config_loaded": self.config is not None,
            "library_available": QONTINUI_AVAILABLE,
        }

    def _cmd_set_debug_settings(self, params: Any) -> dict[str, Any]:
        """Handle the ``set_debug_settings`` command."""
        settings = params.get("settings", {})
        self.executor_core.apply_debug_settings(settings)
        return {"success": True}

    def _cmd_update_capture_settings(self, params: Any) -> dict[str, Any]:
        """Handle the ``update_capture_settings`` command."""
        settings = params.get("settings", {})
        return self.capture_manager.update_settings(settings)  # type: ignore[no-any-return]

    def _cmd_manual_capture_status(self, params: Any) -> dict[str, Any]:
        """Handle the ``manual_capture_status`` command."""
        return {
            "success": True,
            "is_running": self.capture_manager.is_manual_capture_running(),
        }

    def _cmd_set_input_capture_enabled(self, params: Any) -> dict[str, Any]:
        """Handle the ``set_input_capture_enabled`` command."""
        # Enable/disable input capture for coordinate validation during execution
        # When enabled, input will be automatically captured during workflow execution
        enabled = params.get("enabled", False)
        self.capture_input_for_validation = enabled
        self.event_manager.emit_log(
            "info",
            f"Input capture for validation {'enabled' if enabled else 'disabled'}",
        )
        return {"success": True, "enabled": enabled}

    def _cmd_get_input_validation_status(self, params: Any) -> dict[str, Any]:
        """Handle the ``get_input_validation_status`` command."""
        # Get current input validation status
        is_monitoring = (
            self.input_monitor_service is not None and self._input_capture_session_id is not None
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

    def _cmd_test_results_configure(self, params: Any) -> dict[str, Any]:
        """Handle the ``test_results_configure`` command."""
        enabled = params.get("enabled", False)
        api_url = params.get("api_url", "")
        access_token = params.get("access_token", "")
        project_id = params.get("project_id")
        self.event_manager.emit_log(
            "info",
            f"[TEST_RESULTS_CONFIGURE] enabled={enabled}, api_url={api_url}, project_id={project_id}",
        )
        success = self.test_results_handler.configure(enabled, api_url, access_token, project_id)
        return {"success": success}

    def _cmd_test_results_status(self, params: Any) -> dict[str, Any]:
        """Handle the ``test_results_status`` command."""
        return {
            "success": True,
            **self.test_results_handler.get_status(),
        }

    def _cmd_ping(self, params: Any) -> dict[str, Any]:
        """Handle the ``ping`` command."""
        pong_message = {"type": "pong", "timestamp": time.time()}
        print(json.dumps(pong_message), flush=True)
        return {"success": True}

    def _cmd_navigate_to_state(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle the ``navigate_to_state`` command."""
        # Support both "target_state_id" (from Rust action_service) and "state_id" (legacy)
        state_id = params.get("target_state_id") or params.get("state_id")
        if not state_id:
            return {"success": False, "error": "target_state_id (or state_id) is required"}
        return self.navigate_to_state(state_id)

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

            except Exception as e:
                self.event_manager.emit_log("error", f"Remote workflow execution failed: {e}")

        threading.Thread(target=_run_workflow_background, daemon=True).start()

        return {
            "success": True,
            "execution_id": execution_id,
            "status": "started",
            "message": "Workflow execution started",
        }

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


# Built once at import: raises on a duplicate command name or a missing method.
_COMMAND_TABLE: dict[str, CommandEntry] = build_command_table(QontinuiExecutor)


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
                    logger.debug(f"[RESPONSE] {cmd_name}: success={response['success']}")
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
