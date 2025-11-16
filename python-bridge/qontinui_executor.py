#!/usr/bin/env python3
"""
Qontinui executor that integrates with the actual Qontinui library.
"""

import json
import sys
import logging

# CRITICAL: Configure logging to use stderr FIRST before any other imports
# This ensures ALL debug logs go to stderr, keeping stdout clean for JSON events
logging.basicConfig(
    stream=sys.stderr,
    level=logging.DEBUG,
    format='%(asctime)s [%(levelname)s] %(message)s'
)

# CRITICAL: Check for --disable-console-logging flag BEFORE any imports
# This prevents structlog from outputting to stderr/stdout
import os
if "--disable-console-logging" in sys.argv:
    os.environ["QONTINUI_DISABLE_CONSOLE_LOGGING"] = "1"
    # Remove the flag so it doesn't interfere with other parsing
    sys.argv.remove("--disable-console-logging")

# IMMEDIATE debug logging to verify executor is being invoked
import tempfile
from datetime import datetime
debug_log_path = os.path.join(tempfile.gettempdir(), "qontinui_executor_startup.log")
try:
    with open(debug_log_path, "a", encoding="utf-8") as f:
        timestamp = datetime.now().strftime("%Y-%m-%d %H:%M:%S.%f")[:-3]
        f.write(f"[{timestamp}] EXECUTOR STARTED - qontinui_executor.py is running\n")
        f.write(f"[{timestamp}] Console logging disabled: {os.getenv('QONTINUI_DISABLE_CONSOLE_LOGGING') == '1'}\n")
except Exception as e:
    pass

# CRITICAL: Send READY signal IMMEDIATELY to prevent timeout
# Must happen before any other imports that might be slow
# Using sys.stdout.write for immediate output without buffering
sys.stdout.write(json.dumps({
    "type": "ready",
    "data": {"message": "Python executor starting", "library_available": None}
}) + "\n")
sys.stdout.flush()

# Now import remaining modules
import base64
import logging
import os
import tempfile
import threading
import time
import traceback
from enum import Enum
from io import StringIO
from pathlib import Path
from typing import Any, Dict, List, Optional

# Set up logging
logger = logging.getLogger(__name__)

# Add qontinui library src directory to path
# This file is in: qontinui_parent/qontinui-runner/python-bridge/qontinui_executor.py
# We need to add: qontinui_parent/qontinui/src
qontinui_src_path = Path(__file__).parent.parent.parent / "qontinui" / "src"
sys.path.insert(0, str(qontinui_src_path))

# Debug: Log to stderr instead of stdout to avoid polluting JSON event stream
logger.debug(f"Qontinui source path added to sys.path: {qontinui_src_path} (exists: {qontinui_src_path.exists()})")

# CRITICAL: Import local python-bridge modules BEFORE qontinui library
# These modules are always available and needed even if qontinui library fails to import
from event_translator import EventTranslator
from execution_tree import ExecutionTree, ExecutionNode
from action_definitions import get_action_definition
from services.unified_data_collector import UnifiedDataCollector
from services.screenshot_service import ScreenshotService
from services.tree_view_layer import TreeViewLayer
from services.capture_tool_service import CaptureToolService, ScreenshotCaptureSettings, ManualClickListener

# WebSocket integration for qontinui-web
try:
    from websocket_client import RunnerWebSocketClient, authenticate
    from websocket_config import WebSocketConfig
    import asyncio
    WEBSOCKET_AVAILABLE = True
except ImportError as e:
    WEBSOCKET_AVAILABLE = False
    logger.debug(f"WebSocket modules not available: {e}")

try:
    from qontinui import Find, Image, Location
    from qontinui.config import get_settings, enable_mock_mode, disable_mock_mode
    from qontinui import navigation_api, registry
    from qontinui.reporting import register_callback, EventType as QontinuiEventType
    from qontinui.action_executors import DelegatingActionExecutor
    from qontinui.json_executor.config_parser import ConfigParser, QontinuiConfig

    QONTINUI_AVAILABLE = True
except ImportError as e:
    QONTINUI_AVAILABLE = False
    import_error_details = f"{type(e).__name__}: {str(e)}"

    # Get full traceback for debugging
    import traceback
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


class EventType(Enum):
    """Event types for communication with Tauri."""

    READY = "ready"
    CONFIG_LOADED = "config_loaded"
    EXECUTION_STARTED = "execution_started"
    STATE_CHANGED = "state_changed"
    ACTION_STARTED = "action_started"
    ACTION_COMPLETED = "action_completed"
    WORKFLOW_STARTED = "workflow_started"
    WORKFLOW_COMPLETED = "workflow_completed"
    EXECUTION_COMPLETED = "execution_completed"
    ERROR = "error"
    LOG = "log"
    MATCH_FOUND = "match_found"
    SCREENSHOT_TAKEN = "screenshot_taken"
    IMAGE_RECOGNITION = "image_recognition"
    ACTION_EXECUTION = "action_execution"


# ExecutionContext and HierarchyMetadata classes removed - replaced by ExecutionTree


class StateMemoryAdapter:
    """Adapter to bridge StateExecutor interface to UnifiedDataCollector's expected interface.

    StateExecutor provides get_active_states() which returns a list of state names.
    UnifiedDataCollector expects get_active_state_names() for clarity.
    This adapter bridges the interface difference without modifying either component.
    """

    def __init__(self, state_executor):
        """Initialize adapter with StateExecutor instance.

        Args:
            state_executor: StateExecutor instance that provides get_active_states()
        """
        self.state_executor = state_executor

    def get_active_state_names(self) -> list[str]:
        """Get list of active state names.

        Returns:
            List of active state names from StateExecutor
        """
        if self.state_executor is None:
            return []
        return self.state_executor.get_active_states()


class RunnerOrchestrator:
    """
    Pure orchestrator for runner.

    Responsibilities:
    - Load configuration
    - Create StateExecutionAPI (library handles state)
    - Translate Tauri requests to library calls
    - Emit events to Tauri frontend

    Does NOT:
    - Manage states
    - Execute transitions directly
    - Track state memory
    """

    def __init__(self, config_path: str):
        """Initialize orchestrator with configuration.

        Args:
            config_path: Path to JSON configuration file
        """
        self.config_path = config_path
        self.config: Optional[Any] = None
        self.state_executor: Optional[Any] = None
        self.action_executor: Optional[Any] = None
        self._sequence = 0

        # Load configuration
        self.config = self._load_config(config_path)

        # Initialize HAL
        self._initialize_hal()

        logger.info(f"RunnerOrchestrator initialized with config: {config_path}")

    def _load_config(self, config_path: str) -> Any:
        """Load config and register inline workflows.

        Args:
            config_path: Path to JSON configuration file

        Returns:
            QontinuiConfig object
        """
        if not QONTINUI_AVAILABLE:
            raise RuntimeError("Qontinui library not available")

        from qontinui.json_executor.config_parser import ConfigParser
        import json

        with open(config_path, 'r') as f:
            config_dict = json.load(f)

        # Parse config using ConfigParser
        config_parser = ConfigParser()
        config = config_parser.parse_config(config_dict)

        logger.info(f"Config loaded: {len(config.states)} states, {len(config.workflows)} workflows, {len(config.transitions)} transitions")

        # Initialize StateExecutor with config
        from qontinui.json_executor.state_executor import StateExecutor
        self.state_executor = StateExecutor(config)
        self.state_executor.initialize()

        # Get ActionExecutor from StateExecutor
        self.action_executor = self.state_executor.action_executor

        logger.info("StateExecutor and ActionExecutor initialized")

        # Apply any debug settings that were set before config was loaded
        if self.debug_settings:
            self._apply_debug_settings_to_executor()

        return config

    def _initialize_hal(self) -> None:
        """Initialize HAL (Hardware Abstraction Layer)."""
        if not QONTINUI_AVAILABLE:
            return

        # HAL is initialized automatically when qontinui is imported
        # This method exists for future HAL-specific initialization
        logger.debug("HAL initialized")

    def execute_transition(self, transition_id: str) -> Dict[str, Any]:
        """Execute transition via library.

        Args:
            transition_id: ID of transition to execute

        Returns:
            Dict with success status and details
        """
        if not self.state_executor:
            return {
                "success": False,
                "error": "StateExecutor not initialized"
            }

        try:
            # Find transition by ID
            transition = self.config.transition_map.get(transition_id)
            if not transition:
                return {
                    "success": False,
                    "error": f"Transition {transition_id} not found"
                }

            # Execute transition
            success = self.state_executor._execute_transition(transition)

            return {
                "success": success,
                "transition_id": transition_id,
                "active_states": self.state_executor.get_active_states()
            }
        except Exception as e:
            logger.error(f"Failed to execute transition {transition_id}: {e}")
            return {
                "success": False,
                "error": str(e)
            }

    def navigate_to_state(self, target_state_id: str) -> Dict[str, Any]:
        """Navigate to state via library.

        Args:
            target_state_id: ID of target state

        Returns:
            Dict with success status and details
        """
        # DEBUG: Confirm this method is being called
        self._emit_log("info", f"[NAVIGATE] navigate_to_state called with target: {target_state_id}")

        if not QONTINUI_AVAILABLE:
            self._emit_log("error", "[NAVIGATE] Qontinui library not available")
            return {
                "success": False,
                "error": "Qontinui library not available"
            }

        try:
            from qontinui import navigation_api

            # Create a workflow node for this navigation operation
            # This ensures navigation actions appear in the Actions tab
            try:
                nav_node = ExecutionNode(
                    id=f"nav_{self._navigation_sequence}",
                    node_type="workflow",
                    name=f"Navigate to {target_state_id}",
                    timestamp=time.time(),
                    metadata={"target_state": target_state_id},
                    parent=None
                )
                self._navigation_sequence += 1
                self._emit_log("info", f"[NAVIGATE] Created nav_node: {nav_node.id}")
            except Exception as e:
                self._emit_log("error", f"[NAVIGATE] Failed to create ExecutionNode: {e}")
                self._emit_log("error", f"[NAVIGATE] ExecutionNode class: {ExecutionNode}")
                import traceback
                self._emit_log("error", f"[NAVIGATE] Traceback: {traceback.format_exc()}")
                raise

            # Emit workflow_started
            try:
                self._emit_log("info", f"[NAVIGATE] About to emit workflow_started")
                self._emit_tree_event("workflow_started", nav_node)
                self._emit_log("info", f"[NAVIGATE] workflow_started emitted successfully")
            except Exception as e:
                self._emit_log("error", f"[NAVIGATE] Failed to emit tree event: {e}")
                import traceback
                self._emit_log("error", f"[NAVIGATE] Traceback: {traceback.format_exc()}")
                raise

            self._emit_log("info", f"[NAVIGATE] Calling navigation_api.open_state")

            # Use navigation API to navigate to state
            result = navigation_api.open_state(target_state_id)

            self._emit_log("info", f"[NAVIGATE] navigation_api.open_state returned: {result}")

            # Update node status
            success = result.get("success", False) if isinstance(result, dict) else result
            nav_node.status = "completed" if success else "failed"
            if not success:
                nav_node.error = result.get("error", "Navigation failed") if isinstance(result, dict) else "Navigation failed"

            self._emit_log("info", f"[NAVIGATE] Emitting workflow_{'completed' if success else 'failed'}")

            # Emit workflow_completed or workflow_failed
            self._emit_tree_event("workflow_completed" if success else "workflow_failed", nav_node)

            return {
                "success": success,
                "target_state": target_state_id,
                "active_states": self.state_executor.get_active_states() if self.state_executor else [],
                "path": result.get("path", []) if isinstance(result, dict) else []
            }
        except Exception as e:
            logger.error(f"Failed to navigate to state {target_state_id}: {e}")

            # Emit failure event if we created a node
            if 'nav_node' in locals():
                nav_node.status = "failed"
                nav_node.error = str(e)
                self._emit_tree_event("workflow_failed", nav_node)

            return {
                "success": False,
                "error": str(e)
            }

    def navigate_to_multiple_states(self, target_state_ids: List[str]) -> Dict[str, Any]:
        """Navigate to multiple states via library.

        Args:
            target_state_ids: List of target state IDs

        Returns:
            Dict with success status and details
        """
        if not QONTINUI_AVAILABLE:
            return {
                "success": False,
                "error": "Qontinui library not available"
            }

        results = []
        overall_success = True

        for state_id in target_state_ids:
            result = self.navigate_to_state(state_id)
            results.append(result)
            if not result.get("success", False):
                overall_success = False

        return {
            "success": overall_success,
            "results": results,
            "active_states": self.state_executor.get_active_states() if self.state_executor else []
        }

    def get_active_states(self) -> Dict[str, Any]:
        """Get active states from library.

        Returns:
            Dict with active state information
        """
        if not self.state_executor:
            return {
                "success": False,
                "error": "StateExecutor not initialized",
                "active_states": []
            }

        return {
            "success": True,
            "active_states": self.state_executor.get_active_states(),
            "current_state": self.state_executor.current_state,
            "state_history": self.state_executor.get_state_history()
        }

    def get_available_transitions(self) -> Dict[str, Any]:
        """Get available transitions from library.

        Returns:
            Dict with available transition information
        """
        if not self.state_executor:
            return {
                "success": False,
                "error": "StateExecutor not initialized",
                "transitions": []
            }

        try:
            current_state = self.state_executor.current_state
            if not current_state:
                return {
                    "success": True,
                    "transitions": [],
                    "message": "No current state"
                }

            # Get outgoing transitions from current state
            outgoing_transitions = self.state_executor._find_outgoing_transitions(current_state)

            # Format transition information
            transitions = []
            for trans in outgoing_transitions:
                transitions.append({
                    "id": trans.id,
                    "from_state": trans.from_state,
                    "to_state": trans.to_state if hasattr(trans, 'to_state') else None,
                    "workflows": trans.workflows
                })

            return {
                "success": True,
                "transitions": transitions,
                "current_state": current_state
            }
        except Exception as e:
            logger.error(f"Failed to get available transitions: {e}")
            return {
                "success": False,
                "error": str(e),
                "transitions": []
            }

    def _emit_to_tauri(self, event: Dict[str, Any]) -> None:
        """Emit event to Tauri frontend.

        Args:
            event: Event dictionary to emit
        """
        event["timestamp"] = time.time()
        event["sequence"] = self._sequence
        self._sequence += 1
        print(json.dumps(event), flush=True)


class QontinuiExecutor:
    """Executor that uses the Qontinui library for real automation."""

    def __init__(self):
        self.config = None
        self.workflows = {}
        self.images = {}
        self.is_running = False
        self._sequence = 0
        self.temp_dir = None
        self.use_graph_execution = False
        self.qontinui_config = None
        self.mock_mode = "real"  # Track mock mode: "real", "mock", "screenshot"
        self.screenshot_dir = None  # Screenshot directory for screenshot mode
        self.settings = None  # FrameworkSettings instance
        self._last_find_location = None  # Store location of most recent FIND result for "Last Find Result" clicks
        self.action_executor = None  # ActionExecutor instance (initialized in load_configuration)
        self.state_executor = None  # StateExecutor instance (initialized in load_configuration)

        # Debug settings from frontend
        self.debug_settings = {
            'enable_image_debug': False,
            'top_matches_count': 5
        }

        # Thread-safe output lock to prevent JSON concatenation from concurrent threads
        self._output_lock = threading.Lock()

        # Execution tree for hierarchical tracking
        self.execution_tree = ExecutionTree()

        # Unified data architecture services (initialized in load_configuration)
        # These services are created after we have a run directory and state_executor
        self.screenshot_service = None  # ScreenshotService for screenshot storage
        self.unified_data_collector = None  # UnifiedDataCollector for action execution data

        # Screenshot capture tool for configuration builder (qontinui-web)
        self.capture_tool_service = CaptureToolService()  # Initially disabled, enabled via update_capture_settings

        # Manual click listener for capture tool (captures user clicks, not automation clicks)
        self.manual_click_listener = ManualClickListener(self.capture_tool_service)

        # WebSocket client for qontinui-web integration
        self.ws_client: Optional[RunnerWebSocketClient] = None
        self.ws_config: Optional[WebSocketConfig] = None
        self.ws_loop: Optional[asyncio.AbstractEventLoop] = None
        self.ws_thread: Optional[threading.Thread] = None
        self.ws_enabled = False

        # Initialize WebSocket configuration from environment
        if WEBSOCKET_AVAILABLE:
            try:
                self.ws_config = WebSocketConfig.from_env()
                if self.ws_config.enabled:
                    self._emit_log("info", "WebSocket configuration loaded from environment")
            except Exception as e:
                self._emit_log("error", f"Failed to load WebSocket config: {e}")

        if QONTINUI_AVAILABLE:
            # Get framework settings
            self.settings = get_settings()

            # Initialize EventTranslator for callback management
            # EventTranslator handles all event translation from qontinui library to frontend format
            # Pass _get_state_for_image as state_lookup callback to add state info to events
            # Pass _get_current_hierarchy as hierarchy_lookup callback to add hierarchy info to events
            # Pass _get_image_data as image_data_lookup callback to add image thumbnails to events
            # NOTE: UnifiedDataCollector will be passed to EventTranslator in load_configuration
            self.event_translator = EventTranslator(
                self._emit_event_wrapper,
                state_lookup=self._get_state_for_image,
                hierarchy_lookup=self._get_current_hierarchy,
                image_data_lookup=self._get_image_data
            )
            self.event_translator.register_all_callbacks()

            # Register callback for screenshot capture tool (MOUSE_CLICKED events)
            register_callback(QontinuiEventType.MOUSE_CLICKED, self._on_mouse_clicked)

            # Navigation sequence counter for creating unique IDs
            self._navigation_sequence = 0

            # Verify callbacks were registered
            from qontinui.reporting import get_event_registry
            event_registry = get_event_registry()
            self._emit_log("debug", f"[INIT] Event registry has_listeners: {event_registry.has_listeners}")
            self._emit_log("debug", f"[INIT] Event registry MATCH_ATTEMPTED callbacks: {len(event_registry._callbacks.get(QontinuiEventType.MATCH_ATTEMPTED, []))}")
            self._emit_log("debug", f"[INIT] EventTranslator initialized and callbacks registered")

        # READY signal already sent at module import time (before heavy imports)
        # This ensures Rust doesn't timeout waiting for the signal
        self._emit_log("info", f"QontinuiExecutor initialized (library_available={QONTINUI_AVAILABLE})")

    def _emit_event(self, event_type: EventType, data: dict[str, Any]):
        """Emit event to Tauri through stdout (thread-safe)."""
        event = {
            "type": "event",
            "event": event_type.value,
            "timestamp": time.time(),
            "sequence": self._sequence,
            "data": data,
        }
        self._sequence += 1
        # Use lock to prevent concurrent threads from interleaving JSON output
        with self._output_lock:
            print(json.dumps(event), flush=True)

    def _emit_log(self, level: str, message: str):
        """Emit log message."""
        self._emit_event(EventType.LOG, {"level": level, "message": message})

        # Also send to WebSocket if enabled
        if self.ws_enabled and self.ws_client:
            self._ws_send_log(level, message)

    def _ws_send_log(self, level: str, message: str, log_data: Optional[Dict[str, Any]] = None):
        """Send log to WebSocket (non-blocking)."""
        if not self.ws_enabled or not self.ws_client or not self.ws_loop:
            return

        try:
            # Schedule coroutine in WebSocket event loop
            asyncio.run_coroutine_threadsafe(
                self.ws_client.send_log(level, message, log_data),
                self.ws_loop
            )
        except Exception as e:
            logger.error(f"Error sending log to WebSocket: {e}")

    def _ws_send_screenshot(self, image, name: str, metadata: Optional[Dict[str, Any]] = None):
        """Send screenshot to WebSocket (non-blocking)."""
        if not self.ws_enabled or not self.ws_client or not self.ws_loop:
            return

        try:
            # Schedule coroutine in WebSocket event loop
            asyncio.run_coroutine_threadsafe(
                self.ws_client.send_screenshot(image, name, metadata),
                self.ws_loop
            )
        except Exception as e:
            logger.error(f"Error sending screenshot to WebSocket: {e}")

    def _ws_start_event_loop(self):
        """Start WebSocket event loop in background thread."""
        try:
            self.ws_loop = asyncio.new_event_loop()
            asyncio.set_event_loop(self.ws_loop)
            self.ws_loop.run_forever()
        except Exception as e:
            logger.error(f"WebSocket event loop error: {e}")
        finally:
            if self.ws_loop:
                self.ws_loop.close()

    def _ws_connect(self) -> bool:
        """Connect to WebSocket server (blocking until connected)."""
        if not WEBSOCKET_AVAILABLE:
            self._emit_log("error", "WebSocket libraries not available")
            return False

        if not self.ws_config:
            self._emit_log("error", "WebSocket configuration not set")
            return False

        # Validate config
        is_valid, error = self.ws_config.validate()
        if not is_valid:
            self._emit_log("error", f"Invalid WebSocket config: {error}")
            return False

        try:
            # Start event loop in background thread
            if not self.ws_thread or not self.ws_thread.is_alive():
                self.ws_thread = threading.Thread(target=self._ws_start_event_loop, daemon=True)
                self.ws_thread.start()

                # Wait for loop to be ready
                timeout = 5
                start_time = time.time()
                while not self.ws_loop and time.time() - start_time < timeout:
                    time.sleep(0.1)

                if not self.ws_loop:
                    self._emit_log("error", "WebSocket event loop failed to start")
                    return False

            # Get or refresh token
            token = self.ws_config.token
            if not token and self.ws_config.email and self.ws_config.password:
                self._emit_log("info", "Authenticating with qontinui-web...")
                # Convert ws:// to http:// for auth
                auth_url = self.ws_config.api_url.replace("ws://", "http://").replace("wss://", "https://")

                # Run auth in event loop
                future = asyncio.run_coroutine_threadsafe(
                    authenticate(auth_url, self.ws_config.email, self.ws_config.password),
                    self.ws_loop
                )
                token = future.result(timeout=10)

                if not token:
                    self._emit_log("error", "Authentication failed")
                    return False

                self.ws_config.token = token
                self._emit_log("info", "Authentication successful")

            # Create WebSocket client
            self.ws_client = RunnerWebSocketClient(
                api_url=self.ws_config.api_url,
                token=token,
                project_id=self.ws_config.project_id,
                runner_version=self.ws_config.runner_version,
                auto_reconnect=self.ws_config.auto_reconnect,
                heartbeat_interval=self.ws_config.heartbeat_interval,
                max_reconnect_attempts=self.ws_config.max_reconnect_attempts,
                on_connected=lambda: self._emit_log("info", "WebSocket connected"),
                on_disconnected=lambda: self._emit_log("warning", "WebSocket disconnected"),
                on_error=lambda msg: self._emit_log("error", f"WebSocket error: {msg}"),
            )

            # Connect
            self._emit_log("info", f"Connecting to WebSocket: {self.ws_config.api_url}")
            future = asyncio.run_coroutine_threadsafe(
                self.ws_client.connect(),
                self.ws_loop
            )
            success = future.result(timeout=10)

            if success:
                self.ws_enabled = True
                self._emit_log("info", "WebSocket connection established")
                return True
            else:
                self._emit_log("error", "WebSocket connection failed")
                return False

        except Exception as e:
            self._emit_log("error", f"WebSocket connection error: {e}")
            logger.error(f"WebSocket connection error: {traceback.format_exc()}")
            return False

    def _ws_disconnect(self):
        """Disconnect from WebSocket server."""
        if not self.ws_client:
            return

        try:
            self._emit_log("info", "Disconnecting WebSocket...")

            # Disconnect client
            if self.ws_loop and self.ws_client:
                future = asyncio.run_coroutine_threadsafe(
                    self.ws_client.disconnect(),
                    self.ws_loop
                )
                future.result(timeout=5)

            # Stop event loop
            if self.ws_loop:
                self.ws_loop.call_soon_threadsafe(self.ws_loop.stop)

            self.ws_enabled = False
            self.ws_client = None

            self._emit_log("info", "WebSocket disconnected")

        except Exception as e:
            self._emit_log("error", f"Error disconnecting WebSocket: {e}")
            logger.error(f"WebSocket disconnect error: {traceback.format_exc()}")

    def _ws_start_session(self, config_snapshot: Optional[Dict[str, Any]] = None) -> bool:
        """Start WebSocket session."""
        if not self.ws_enabled or not self.ws_client or not self.ws_loop:
            self._emit_log("warning", "Cannot start WebSocket session: not connected")
            return False

        try:
            self._emit_log("info", "Starting WebSocket session...")

            future = asyncio.run_coroutine_threadsafe(
                self.ws_client.start_session(config_snapshot),
                self.ws_loop
            )
            success = future.result(timeout=10)

            if success:
                self._emit_log("info", f"WebSocket session started: {self.ws_client.session_id}")
                return True
            else:
                self._emit_log("error", "Failed to start WebSocket session")
                return False

        except Exception as e:
            self._emit_log("error", f"Error starting WebSocket session: {e}")
            logger.error(f"WebSocket session start error: {traceback.format_exc()}")
            return False

    def _ws_end_session(self, status: str = "completed", error: Optional[str] = None) -> bool:
        """End WebSocket session."""
        if not self.ws_enabled or not self.ws_client or not self.ws_loop:
            return False

        try:
            self._emit_log("info", f"Ending WebSocket session with status: {status}")

            future = asyncio.run_coroutine_threadsafe(
                self.ws_client.end_session(status, error),
                self.ws_loop
            )
            success = future.result(timeout=10)

            if success:
                self._emit_log("info", "WebSocket session ended")
            else:
                self._emit_log("error", "Failed to end WebSocket session")

            return success

        except Exception as e:
            self._emit_log("error", f"Error ending WebSocket session: {e}")
            logger.error(f"WebSocket session end error: {traceback.format_exc()}")
            return False

    def set_debug_settings(self, settings: dict):
        """Update debug settings from frontend.

        Args:
            settings: Dictionary containing debug configuration:
                - enable_image_debug: bool - Enable detailed image matching debug info
                - top_matches_count: int - Number of top matches to report
        """
        self.debug_settings = settings
        self._emit_log("info", f"Debug settings updated: {settings}")

        # Apply settings to ActionExecutor if available
        if QONTINUI_AVAILABLE and self.action_executor:
            self._apply_debug_settings_to_executor()

    def _apply_debug_settings_to_executor(self):
        """Apply current debug settings to the FrameworkSettings singleton."""
        if not QONTINUI_AVAILABLE:
            return

        try:
            # Import FrameworkSettings to update the singleton
            from qontinui.config.framework_settings import FrameworkSettings

            # Get the singleton instance
            settings = FrameworkSettings.get_instance()

            # Set emit_match_details based on debug settings
            enable_debug = self.debug_settings.get('enable_image_debug', False)
            settings.image_debug.emit_match_details = enable_debug

            # Verify it was actually set by reading it back
            actual_value = settings.image_debug.emit_match_details

            self._emit_log(
                "info",
                f"Applied debug settings to FrameworkSettings: emit_match_details={enable_debug}, verified={actual_value}"
            )

        except Exception as e:
            self._emit_log("error", f"Failed to apply debug settings: {e}")

    def _initialize_unified_data_services(self):
        """Initialize unified data architecture services.

        This method sets up:
        1. ScreenshotService with run directory
        2. StateMemoryAdapter to bridge StateExecutor to UnifiedDataCollector
        3. UnifiedDataCollector with state memory adapter and screenshot service

        Integration point: Called after StateExecutor initialization in load_configuration
        """
        if not QONTINUI_AVAILABLE:
            return

        try:
            # Create run directory for screenshots (use temp_dir if available, else create new)
            if self.temp_dir:
                run_dir = Path(self.temp_dir) / "run_data"
            else:
                run_dir = Path(tempfile.mkdtemp(prefix="qontinui_run_"))

            run_dir.mkdir(parents=True, exist_ok=True)

            # Initialize ScreenshotService
            # Storage structure: run_dir/screenshots/ and run_dir/debug_visuals/
            self.screenshot_service = ScreenshotService(
                storage_dir=run_dir,
                enabled=True  # Always enabled for data collection
            )
            self._emit_log("info", f"ScreenshotService initialized with storage: {run_dir}")

            # Create state memory adapter
            # StateExecutor has get_active_states() but UnifiedDataCollector expects get_active_state_names()
            # Adapter bridges this interface difference
            state_memory_adapter = StateMemoryAdapter(self.state_executor)

            # Initialize UnifiedDataCollector
            # Pass state memory adapter for state snapshot capture
            # Pass screenshot_service for screenshot storage integration
            self.unified_data_collector = UnifiedDataCollector(
                state_memory=state_memory_adapter,
                screenshot_service=self.screenshot_service
            )
            self._emit_log("info", "UnifiedDataCollector initialized with StateExecutor adapter and ScreenshotService")

            # Connect UnifiedDataCollector to EventTranslator
            # This enables the translator to record execution data (typed text, match results, etc.)
            # The translator operates in dual mode: records to collector AND emits to frontend
            if hasattr(self, 'event_translator') and self.event_translator:
                self.event_translator.collector = self.unified_data_collector
                self._emit_log("info", "EventTranslator connected to UnifiedDataCollector")

        except Exception as e:
            self._emit_log("error", f"Failed to initialize unified data services: {e}")
            self._emit_log("debug", f"Traceback: {traceback.format_exc()}")
            # Set to None so we can check availability
            self.screenshot_service = None
            self.unified_data_collector = None

    def _emit_tree_event(self, event_type: str, node: "ExecutionNode", extra_data: dict = None):
        """Emit a tree-based event with full tree context.

        Args:
            event_type: Type of event (e.g., "workflow_started", "action_completed")
            node: The execution node this event relates to
            extra_data: Optional additional data to include in the event
        """
        node_dict = node.to_dict(include_children=False)

        # Add nesting_level directly to the node dict for workflows
        # Actions already have this in their execution_record, but workflows don't
        if "nesting_level" not in node_dict:
            node_dict["nesting_level"] = node.get_depth()

        # Workflows should be expandable (they contain actions)
        # Actions have is_expandable in their metadata from action definitions
        if node.node_type == "workflow":
            if "metadata" not in node_dict:
                node_dict["metadata"] = {}
            if isinstance(node_dict["metadata"], dict):
                node_dict["metadata"]["is_expandable"] = True

        event_data = {
            "type": "tree_event",
            "event_type": event_type,
            "node": node_dict,
            "timestamp": time.time(),
            "sequence": self._sequence,
        }
        self._sequence += 1

        # Include path from root for easy breadcrumb display
        event_data["path"] = node.get_path_from_root()

        # Merge in extra data if provided
        if extra_data:
            event_data.update(extra_data)

        # Use lock to prevent concurrent threads from interleaving JSON output
        with self._output_lock:
            print(json.dumps(event_data), flush=True)

    def _emit_event_wrapper(self, event_type: str, data: dict[str, Any]):
        """Wrapper for EventTranslator to convert string event names to EventType enum.

        This method acts as a bridge between EventTranslator (which uses string event names)
        and _emit_event (which expects EventType enum values).

        Also forwards automation events to WebSocket backend for real-time monitoring.

        Args:
            event_type: String event type name (e.g., "image_recognition", "action_execution")
            data: Event data dictionary
        """
        # IMAGE_RECOGNITION events get their own message type (not wrapped as Event)
        if event_type == "image_recognition":
            event = {
                "type": "image_recognition",
                "data": data,
            }
            with self._output_lock:
                print(json.dumps(event), flush=True)

            # Forward image recognition events to WebSocket
            if self.ws_enabled and self.ws_client:
                # Extract screenshot if available and send via WebSocket
                screenshot_base64 = data.get("screenshot_base64")
                if screenshot_base64:
                    # Create metadata for screenshot
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
                    # Send screenshot (non-blocking)
                    self._ws_send_screenshot(
                        image=screenshot_base64,
                        name=f"match_{data.get('image_id', 'unknown')}_{int(time.time())}",
                        metadata=metadata
                    )

                # Also send as structured log
                self._ws_send_log(
                    level="info" if data.get("found") else "debug",
                    message=f"Image recognition: {data.get('image_id')} - {'found' if data.get('found') else 'not found'}",
                    log_data={
                        "event_type": "image_recognition",
                        "image_id": data.get("image_id"),
                        "found": data.get("found"),
                        "confidence": data.get("confidence"),
                        "threshold": data.get("threshold"),
                        "match_location": data.get("match_location"),
                    }
                )
            return

        # Map string event names to EventType enum values
        event_type_map = {
            "image_recognition": EventType.IMAGE_RECOGNITION,
            "action_execution": EventType.ACTION_EXECUTION,
            "action_started": EventType.ACTION_STARTED,
            "action_completed": EventType.ACTION_COMPLETED,
            "match_found": EventType.MATCH_FOUND,
            "screenshot_taken": EventType.SCREENSHOT_TAKEN,
            "log": EventType.LOG,
        }

        # Get the EventType enum value, defaulting to LOG if not found
        enum_event_type = event_type_map.get(event_type, EventType.LOG)

        # If we had to fallback to LOG, add a prefix to the data
        if enum_event_type == EventType.LOG and event_type not in event_type_map:
            data = {**data, "original_event_type": event_type}

        # Forward automation events to WebSocket
        if self.ws_enabled and self.ws_client:
            # Determine log level based on event type
            log_level_map = {
                "action_started": "info",
                "action_completed": "info",
                "action_execution": "info",
                "match_found": "info",
                "screenshot_taken": "debug",
                "log": data.get("level", "info"),
            }
            level = log_level_map.get(event_type, "info")

            # Create message based on event type
            if event_type == "action_started":
                message = f"Action started: {data.get('action_type', 'unknown')}"
            elif event_type == "action_completed":
                success = data.get("success", True)
                action_type = data.get("action_type", 'unknown')
                message = f"Action {'completed' if success else 'failed'}: {action_type}"
                if not success:
                    level = "error"
            elif event_type == "action_execution":
                message = f"Executed: {data.get('action_type', 'unknown')}"
            elif event_type == "match_found":
                message = f"Match found: {data.get('image_id', 'unknown')}"
            elif event_type == "screenshot_taken":
                message = f"Screenshot captured: {data.get('reference', 'unknown')}"
            else:
                message = data.get("message", f"Event: {event_type}")

            # Send to WebSocket with full event data as structured log
            self._ws_send_log(
                level=level,
                message=message,
                log_data={
                    "event_type": event_type,
                    **data  # Include all event data
                }
            )

        # Emit the event using the existing method
        self._emit_event(enum_event_type, data)

    # NOTE: _capture_qontinui_output() and _parse_and_emit_qontinui_log() have been REMOVED.
    # The log parsing infrastructure has been replaced with structured events from the library.
    # Events are now emitted directly by the library via EventTranslator callbacks (line ~141):
    # - TEXT_TYPED events: handled by EventTranslator.on_text_typed()
    # - MATCH_ATTEMPTED events: handled by EventTranslator.on_match_attempted()
    # The library uses standard Python logging which doesn't output to stdout by default.

    def _get_current_hierarchy(self) -> dict[str, Any]:
        """Get current execution hierarchy from execution tree.

        This callback is used by EventTranslator to add hierarchy information
        to events emitted by the library's ActionExecutor.

        Returns:
            Dictionary with hierarchy fields: parent_id, nesting_level, workflow_name, is_expandable
        """
        # Get hierarchy from execution tree
        return self.execution_tree.get_current_hierarchy()

    def _get_hierarchy_context(self) -> dict[str, Any]:
        """Extract hierarchy context for data collection.

        This method extracts hierarchy information from the execution tree
        for use with UnifiedDataCollector.start_action().

        Returns:
            Dictionary with:
                - parent_action_id: ID of parent action/workflow (None for root)
                - workflow_id: ID of containing workflow
                - nesting_level: Depth in execution hierarchy (0 for root)
        """
        if not self.execution_tree or not self.execution_tree.current:
            return {
                "parent_action_id": None,
                "workflow_id": None,
                "nesting_level": 0
            }

        current = self.execution_tree.current

        # Parent is the current node (action will be added as child)
        parent_action_id = current.id

        # Find workflow by traversing up the tree
        workflow_id = None
        node = current
        while node:
            if node.node_type == "workflow":
                workflow_id = node.id
                break
            node = node.parent

        # Nesting level is current depth + 1 (for the new child)
        nesting_level = current.get_depth() + 1

        return {
            "parent_action_id": parent_action_id,
            "workflow_id": workflow_id,
            "nesting_level": nesting_level
        }

    def _on_mouse_clicked(self, event) -> None:
        """Handle MOUSE_CLICKED event for screenshot capture tool.

        Args:
            event: Mouse clicked event from qontinui library
        """
        if self.capture_tool_service and hasattr(event, 'x') and hasattr(event, 'y'):
            button = getattr(event, 'button', 'left')
            self.capture_tool_service.on_click(event.x, event.y, button)

    def update_capture_settings(self, settings_dict: dict[str, Any]) -> dict[str, Any]:
        """Update screenshot capture tool settings.

        Automatically starts/stops the manual click listener based on settings.

        Args:
            settings_dict: Dictionary with capture settings:
                - enabled: bool - Enable/disable capture
                - manualClicksEnabled: bool - Enable/disable manual click capture
                - output_folder: str - Where to save screenshots
                - base_image_name: str - Base name for screenshot files
                - screens: dict - Screen selection configuration
                - capture_timings: list[int] - Capture delays in milliseconds

        Returns:
            Dict with success status
        """
        self._emit_log("info", f"update_capture_settings called: enabled={settings_dict.get('enabled')}, manualClicksEnabled={settings_dict.get('manualClicksEnabled')}")
        try:
            settings = ScreenshotCaptureSettings.from_dict(settings_dict)
            self._emit_log("info", f"Parsed settings: enabled={settings.enabled}, manual_clicks_enabled={settings.manual_clicks_enabled}")
            self._emit_log("info", f"Output folder: {settings.output_folder}")

            self.capture_tool_service.update_settings(settings)

            # Start or stop manual click listener based on manual_clicks_enabled setting
            if settings.manual_clicks_enabled and settings.enabled:
                self._emit_log("info", "Attempting to start manual click listener")
                if not self.manual_click_listener.is_running():
                    self._emit_log("info", "Starting listener...")
                    success = self.manual_click_listener.start()
                    self._emit_log("info", f"Listener start result: {success}")
                    if success:
                        self._emit_log("info", "Manual click capture started successfully")
                    else:
                        self._emit_log("warning", "Failed to start manual click capture")
                else:
                    self._emit_log("info", "Listener already running")
            else:
                self._emit_log("info", f"Manual clicks disabled or capture disabled")
                if self.manual_click_listener.is_running():
                    self._emit_log("info", "Stopping listener")
                    self.manual_click_listener.stop()
                    self._emit_log("info", "Manual click capture stopped")

            is_running = self.manual_click_listener.is_running()
            self._emit_log("info", f"Final state: listener is_running={is_running}")

            # Emit event with manual capture status for frontend
            self._emit_event(EventType.LOG, {
                "level": "info",
                "message": "capture_status_update",
                "manual_capture_running": is_running
            })

            return {"success": True, "manual_capture_running": is_running}
        except Exception as e:
            self._emit_log("error", f"update_capture_settings failed: {e}")
            import traceback
            self._emit_log("error", f"Traceback: {traceback.format_exc()}")
            return {"success": False, "error": str(e)}

    def _get_state_for_image(self, image_id: str) -> str | None:
        """Find which state an image belongs to.

        Args:
            image_id: ID of the image to check

        Returns:
            State name if found, None otherwise
        """
        if not self.config:
            self._emit_log("debug", f"_get_state_for_image: No config available for image {image_id}")
            return None

        states = self.config.get("states", [])
        self._emit_log("debug", f"_get_state_for_image: Checking {len(states)} states for image {image_id}")

        for state in states:
            state_name = state.get("name")
            state_images = state.get("stateImages", [])

            for state_image in state_images:
                state_image_id = state_image.get("id")
                state_image_name = state_image.get("name")

                # Check if this is the state image ID
                if state_image_id == image_id:
                    self._emit_log("debug", f"_get_state_for_image: Found image {image_id} as state image in state '{state_name}'")
                    return state_name

                # Check if this image is used in any pattern
                patterns = state_image.get("patterns", [])
                for pattern in patterns:
                    pattern_image_id = pattern.get("image")
                    if pattern_image_id == image_id:
                        self._emit_log("debug", f"_get_state_for_image: Found image {image_id} in pattern of state image '{state_image_name}' in state '{state_name}'")
                        return state_name

        self._emit_log("debug", f"_get_state_for_image: Image {image_id} not found in any state")
        return None

    def _get_image_name(self, image_id: str) -> str | None:
        """Get the human-readable name for an image ID.

        Args:
            image_id: ID of the image to look up

        Returns:
            Image name if found, None otherwise
        """
        if not self.config:
            return None

        states = self.config.get("states", [])

        for state in states:
            state_images = state.get("stateImages", [])

            for state_image in state_images:
                state_image_id = state_image.get("id")
                state_image_name = state_image.get("name")

                # Check if this is the state image ID
                if state_image_id == image_id:
                    return state_image_name

                # Check if this image is used in any pattern
                patterns = state_image.get("patterns", [])
                for pattern in patterns:
                    pattern_image_id = pattern.get("image")
                    if pattern_image_id == image_id:
                        # Return the state image name (parent) for pattern images
                        return state_image_name

        return None

    def _get_image_data(self, image_id: str) -> str | None:
        """Get base64 image data for an image ID.

        Args:
            image_id: ID of the image to retrieve

        Returns:
            Base64 image data string if found, None otherwise
        """
        if not self.config:
            return None

        images = self.config.get("images", [])
        for image in images:
            if image.get("id") == image_id:
                # Return the base64 data
                return image.get("data")

        return None

    def _get_best_match_regardless_of_threshold(self, image_id: str) -> dict:
        """Get best match info even if it doesn't meet threshold.

        Args:
            image_id: ID of the image to search for

        Returns:
            Dict with 'confidence', 'x', 'y' or None if matching fails
        """
        if image_id not in self.images:
            return None

        try:
            import cv2
            import numpy as np
            from PIL import ImageGrab

            # Get template image from Image object
            image_obj = self.images[image_id]

            # Use Image object's BGR conversion method
            template = image_obj.get_mat_bgr()
            if template is None:
                self._emit_log("debug", f"Could not convert image {image_id} to BGR format")
                return None

            # Capture screenshot
            screenshot = ImageGrab.grab()
            screenshot_cv = cv2.cvtColor(np.array(screenshot), cv2.COLOR_RGB2BGR)

            # Perform template matching
            result = cv2.matchTemplate(screenshot_cv, template, cv2.TM_CCOEFF_NORMED)
            min_val, max_val, min_loc, max_loc = cv2.minMaxLoc(result)

            # Get template dimensions to calculate center
            h, w = template.shape[:2]
            center_x = max_loc[0] + w // 2
            center_y = max_loc[1] + h // 2

            return {"confidence": float(max_val), "x": center_x, "y": center_y}

        except Exception as e:
            self._emit_log("debug", f"Could not get best match: {e}")
            return None

    def _emit_image_recognition_event(
        self, image_id: str, matches: list, threshold: float = 0.9, best_match_info: dict = None
    ):
        """Emit image recognition event with detailed information.

        Args:
            image_id: ID of the image being searched for
            matches: List of matches found (empty list if not found)
            threshold: Similarity threshold used for matching
            best_match_info: Optional dict with best match info even if it didn't meet threshold
                           Should contain: 'confidence', 'x', 'y'
        """
        self._emit_log(
            "debug",
            f"_emit_image_recognition_event called for image: {image_id}, matches: {len(matches) if matches else 0}",
        )

        if image_id not in self.images:
            self._emit_log("warning", f"Image {image_id} not in loaded images")
            return

        # Get image information
        image_obj = self.images[image_id]

        # Get display name for the image (prefer name over ID)
        display_name = image_id  # Default to ID
        if hasattr(image_obj, "name") and image_obj.name:
            display_name = image_obj.name
        elif isinstance(image_obj, dict) and "name" in image_obj:
            display_name = image_obj["name"]

        # Try to get template size from Image object
        template_size = ""
        try:
            if hasattr(image_obj, "mat") and image_obj.mat is not None:
                # OpenCV mat format: (height, width, channels)
                template_size = f"{image_obj.mat.shape[1]}, {image_obj.mat.shape[0]}"
            elif hasattr(image_obj, '_pattern') and hasattr(image_obj._pattern, 'mat'):
                template_size = f"{image_obj._pattern.mat.shape[1]}, {image_obj._pattern.mat.shape[0]}"
            elif hasattr(image_obj, "width") and hasattr(image_obj, "height"):
                template_size = f"{image_obj.width}, {image_obj.height}"
        except Exception as e:
            self._emit_log("debug", f"Could not get template size: {e}")

        # Try to get screenshot size
        screenshot_size = "1920, 1080"  # Default from screen capture
        try:
            from PIL import ImageGrab

            screenshot = ImageGrab.grab()
            screenshot_size = f"{screenshot.width}, {screenshot.height}"
        except Exception:
            pass

        # Get state information for this image
        state_name = self._get_state_for_image(image_id)

        if matches:
            # Get confidence from first match
            first_match = matches[0]
            confidence = getattr(first_match, "score", threshold)
            location = f"({getattr(first_match, 'x', 0)}, {getattr(first_match, 'y', 0)})"

            # Emit event for successful match
            self._emit_log(
                "debug",
                f"[EXECUTOR] Values: threshold={threshold}, confidence={confidence}",
            )

            event_data = {
                "image_path": display_name,
                "template_size": template_size,
                "screenshot_size": screenshot_size,
                "threshold": threshold,  # Send raw 0.0-1.0 value
                "confidence": confidence,  # Send raw 0.0-1.0 value
                "found": True,
                "location": location,
                "gap": threshold - confidence if confidence < threshold else 0,
                "percent_off": (
                    ((threshold - confidence) / threshold) if confidence < threshold else 0
                ),
            }

            # Add state information if available
            if state_name:
                event_data["state"] = state_name

            self._emit_log(
                "debug",
                f"Emitting IMAGE_RECOGNITION event (FOUND): {image_id}, threshold={threshold}, confidence={confidence}, state: {state_name or 'N/A'}",
            )
            self._emit_event(EventType.IMAGE_RECOGNITION, event_data)
        else:
            # Build event data for no match found
            self._emit_log(
                "debug",
                f"[EXECUTOR] Threshold for NOT FOUND: {threshold}",
            )

            event_data = {
                "image_path": display_name,
                "template_size": template_size,
                "screenshot_size": screenshot_size,
                "threshold": threshold,  # Send raw 0.0-1.0 value
                "confidence": 0.0,
                "found": False,
            }

            # Add state information if available
            if state_name:
                event_data["state"] = state_name

            # Add best match information if available
            if best_match_info:
                best_confidence = best_match_info.get("confidence", 0.0)
                best_x = best_match_info.get("x", 0)
                best_y = best_match_info.get("y", 0)

                self._emit_log(
                    "debug",
                    f"[EXECUTOR] Best match confidence: {best_confidence}",
                )

                event_data["confidence"] = best_confidence  # Send raw 0.0-1.0 value
                event_data["best_match_location"] = f"({best_x}, {best_y})"
                event_data["gap"] = threshold - best_confidence
                event_data["percent_off"] = (
                    ((threshold - best_confidence) / threshold) if threshold > 0 else 0
                )

            # Emit event
            self._emit_log(
                "debug",
                f"Emitting IMAGE_RECOGNITION event (NOT FOUND): {image_id}, threshold={threshold}, confidence={event_data.get('confidence', 0)}, best_match: {best_match_info is not None}, state: {state_name or 'N/A'}",
            )
            self._emit_event(EventType.IMAGE_RECOGNITION, event_data)

    def _process_special_keys(self, text: str) -> str:
        """Process special key placeholders in text.

        Converts placeholders like {ENTER}, {TAB}, etc. to actual key values.
        """
        # Simple mappings for common special keys
        replacements = {
            "{ENTER}": "\n",
            "{TAB}": "\t",
            "{SPACE}": " ",
            "{BACKSPACE}": "\b",
            # For complex keys, we'll need to handle them separately
            # For now, just remove the placeholders
            "{DELETE}": "",  # TODO: Handle DELETE key
            "{ESCAPE}": "",  # TODO: Handle ESCAPE key
            "{UP}": "",  # TODO: Handle arrow keys
            "{DOWN}": "",
            "{LEFT}": "",
            "{RIGHT}": "",
            "{HOME}": "",
            "{END}": "",
            "{PAGE_UP}": "",
            "{PAGE_DOWN}": "",
            "{INSERT}": "",
            # Function keys
            "{F1}": "",
            "{F2}": "",
            "{F3}": "",
            "{F4}": "",
            "{F5}": "",
            "{F6}": "",
            "{F7}": "",
            "{F8}": "",
            "{F9}": "",
            "{F10}": "",
            "{F11}": "",
            "{F12}": "",
            # Key combos - these need special handling
            "{CTRL+A}": "",  # TODO: Handle key combinations
            "{CTRL+C}": "",
            "{CTRL+V}": "",
            "{CTRL+X}": "",
            "{CTRL+Z}": "",
            "{CTRL+S}": "",
            "{ALT+TAB}": "",
            "{ALT+F4}": "",
        }

        result = text
        for placeholder, replacement in replacements.items():
            result = result.replace(placeholder, replacement)

        # Log if we had to skip any complex keys
        if any(
            key in text
            for key in [
                "{DELETE}",
                "{ESCAPE}",
                "{UP}",
                "{DOWN}",
                "{CTRL+",
                "{ALT+",
                "{F1",
                "{F2",
                "{F3",
            ]
        ):
            self._emit_log(
                "warning", "Some special keys are not yet fully supported and were skipped"
            )

        return result

    def load_configuration(self, config_path: str) -> bool:
        """Load configuration from file and set up Qontinui states."""
        try:
            self._emit_log("info", f"Loading configuration from: {config_path}")

            with open(config_path) as f:
                self.config = json.load(f)

            # Note: We allow config loading even without Qontinui library for testing
            # Actual execution will still require the library
            if not QONTINUI_AVAILABLE:
                self._emit_log(
                    "warning",
                    "Qontinui library not available - config loaded but execution will not work",
                )

            # Create temp directory for images
            self.temp_dir = tempfile.mkdtemp(prefix="qontinui_")

            # Process images - save to temp files and register in library
            for img_data in self.config.get("images", []):
                img_id = img_data.get("id")
                img_base64 = img_data.get("data", "")
                img_name = img_data.get("name", f"{img_id}.png")

                # Decode base64 and save to temp file
                img_path = os.path.join(self.temp_dir, img_name)
                try:
                    img_bytes = base64.b64decode(img_base64)
                    with open(img_path, "wb") as f:
                        f.write(img_bytes)

                    # Create Qontinui Image object if library is available
                    if QONTINUI_AVAILABLE:
                        image_obj = Image.from_file(img_path)

                        # Validate that the image loaded successfully
                        if image_obj.is_empty():
                            self._emit_log("error", f"Image {img_id} failed to load - PIL image is empty (width=0, height=0)")
                            self._emit_log("error", f"Check if base64 data is valid for image: {img_name}")
                            # Still register it so we don't crash, but it won't work
                        else:
                            self._emit_log("debug", f"Loaded and registered image: {img_id} -> {img_path} ({image_obj.width}x{image_obj.height})")

                        self.images[img_id] = image_obj
                        # Register image in library's registry with metadata for state/transition loading
                        registry.register_image(img_id, image_obj, file_path=img_path, name=img_name)
                    else:
                        # Store path for testing purposes
                        self.images[img_id] = {"path": img_path}
                        self._emit_log("debug", f"Loaded image: {img_id} -> {img_path}")
                except Exception as e:
                    self._emit_log("error", f"Failed to load image {img_id}: {e}")

            # Load state images - map state image IDs to their underlying image objects
            # State images are used by IF actions in inline workflows to check state visibility
            if QONTINUI_AVAILABLE:
                states = self.config.get("states", [])
                self._emit_log("debug", f"Loading state images from {len(states)} states")

                for state in states:
                    state_name = state.get("name", "unknown")
                    state_images = state.get("stateImages", [])  # Config uses 'stateImages' not 'images'
                    self._emit_log("debug", f"State '{state_name}' has {len(state_images)} state images")

                    for state_image in state_images:
                        state_image_id = state_image.get("id")
                        state_image_name = state_image.get("name", "unknown")
                        patterns = state_image.get("patterns", [])

                        self._emit_log("debug", f"Processing state image '{state_image_name}' (id={state_image_id}) with {len(patterns)} patterns")

                        if patterns and len(patterns) > 0:
                            # Get the first pattern's image ID
                            first_pattern = patterns[0]
                            # NEW: Use imageId field (proper reference), fallback to image field (legacy embedded data)
                            underlying_image_id = first_pattern.get("imageId") or first_pattern.get("image")

                            self._emit_log("debug", f"State image {state_image_id} -> underlying image {underlying_image_id}")

                            if underlying_image_id and underlying_image_id in self.images:
                                # Map state image ID to the underlying image object
                                self.images[state_image_id] = self.images[underlying_image_id]
                                # Also register the state image ID in the library's registry
                                # so IF conditions can find it - copy metadata from underlying image
                                underlying_metadata = registry.get_image_metadata(underlying_image_id)
                                if underlying_metadata:
                                    registry.register_image(
                                        state_image_id,
                                        self.images[underlying_image_id],
                                        file_path=underlying_metadata.get("file_path"),
                                        name=underlying_metadata.get("name")
                                    )
                                else:
                                    registry.register_image(state_image_id, self.images[underlying_image_id])
                                self._emit_log("debug", f"Mapped and registered state image {state_image_id} -> {underlying_image_id}")
                            else:
                                self._emit_log("warning", f"State image {state_image_id} references missing image {underlying_image_id}")
                        else:
                            self._emit_log("warning", f"State image {state_image_id} has no patterns")

            # Note: State management is handled by the Qontinui library internally
            # The runner does not need to create or manage states

            # Load execution mode configuration (REAL, MOCK, or SCREENSHOT)
            execution_settings = self.config.get("settings", {}).get("execution", {})
            exec_mode_str = execution_settings.get("executionMode", "real").lower()
            screenshot_dir = execution_settings.get("screenshotDirectory")

            # Parse execution mode from config and update FrameworkSettings
            if QONTINUI_AVAILABLE:
                try:
                    # Store mode and screenshot dir
                    self.mock_mode = exec_mode_str
                    self.screenshot_dir = screenshot_dir

                    # Update FrameworkSettings based on mode
                    if exec_mode_str == "mock":
                        enable_mock_mode()
                        self._emit_log("info", "Mock mode enabled via FrameworkSettings")
                    elif exec_mode_str == "screenshot":
                        # Screenshot mode: enable mock and set screenshot path
                        enable_mock_mode()
                        if screenshot_dir and self.settings:
                            self.settings.screenshot_path = screenshot_dir
                            self.settings.save_snapshots = True
                        self._emit_log("info", f"Screenshot mode enabled, directory: {screenshot_dir}")
                    else:  # real mode
                        disable_mock_mode()
                        self._emit_log("info", "Real execution mode enabled")

                except Exception as e:
                    self._emit_log(
                        "warning",
                        f"Failed to initialize execution mode: {e}. Defaulting to REAL mode.",
                    )
                    self.mock_mode = "real"
                    disable_mock_mode()

            # Check for graph execution setting (v2.0.0)
            # Note: Graph execution not available - json_executor modules don't exist in qontinui
            self.use_graph_execution = False
            if execution_settings.get("useGraphExecution", False):
                self._emit_log(
                    "warning",
                    "Graph execution requested but not available (json_executor modules don't exist). Using sequential execution.",
                )

            # Process workflows and register in library
            workflow_data = self.config.get("workflows", [])
            for workflow in workflow_data:
                workflow_id = workflow.get("id")
                workflow_name = workflow.get("name", workflow_id)
                actions = workflow.get("actions", [])
                # Store both actions and name for hierarchical logging
                self.workflows[workflow_id] = {
                    "actions": actions,
                    "name": workflow_name
                }

                # Register workflow in library's registry for transition loading
                if QONTINUI_AVAILABLE:
                    registry.register_workflow(workflow_id, actions, workflow_name)
                    self._emit_log("debug", f"Registered workflow: {workflow_name}")

            # Initialize navigation system with config (if library is available)
            # This must happen AFTER images and workflows are registered
            if QONTINUI_AVAILABLE:
                try:
                    success = navigation_api.load_configuration(self.config)
                    if success:
                        self._emit_log("info", "Navigation system initialized with states and transitions")
                    else:
                        self._emit_log("error", "Failed to initialize navigation system - check configuration")

                    # CRITICAL: Inject the runner as workflow executor so transitions can execute workflows
                    # This must happen after load_configuration, even if there were non-critical errors
                    # The navigator may have been partially initialized and needs the executor
                    navigation_api.set_workflow_executor(self)
                    self._emit_log("info", "Runner injected as workflow_executor for navigation transitions")

                except Exception as e:
                    self._emit_log("warning", f"Failed to initialize navigation: {e}")
                    self._emit_log("debug", f"Traceback: {traceback.format_exc()}")
                    # Even after an exception, attempt to set the workflow executor if navigator exists
                    try:
                        navigation_api.set_workflow_executor(self)
                        self._emit_log("info", "Runner injected as workflow_executor (after exception recovery)")
                    except Exception as executor_error:
                        self._emit_log("debug", f"Could not set workflow executor: {executor_error}")

            # Parse config into QontinuiConfig and initialize StateExecutor
            # This must happen AFTER navigation is initialized
            if QONTINUI_AVAILABLE:
                try:
                    # Parse JSON config into QontinuiConfig object
                    config_parser = ConfigParser()
                    self.qontinui_config = config_parser.parse_config(self.config)
                    self._emit_log("info", f"Parsed config into QontinuiConfig: {len(self.qontinui_config.workflows)} workflows, {len(self.qontinui_config.states)} states")

                    # Sync inline workflows from registry to workflow_map
                    # Inline workflows are registered during transition loading but not in config.workflows[]
                    # Add them to workflow_map so StateExecutor can find them
                    from qontinui.config.schema import Workflow, WorkflowVisibility

                    registry_workflow_ids = registry.get_all_workflow_ids()
                    inline_workflow_count = 0
                    failed_workflow_count = 0
                    for workflow_id in registry_workflow_ids:
                        if workflow_id not in self.qontinui_config.workflow_map:
                            # This is an inline workflow - get its definition from registry
                            workflow_def = registry.get_workflow_definition(workflow_id)
                            if workflow_def:
                                # Mark as INTERNAL and validate against schema
                                workflow_def['visibility'] = WorkflowVisibility.INTERNAL.value
                                try:
                                    workflow_obj = Workflow(**workflow_def)
                                    self.qontinui_config.workflow_map[workflow_id] = workflow_obj
                                    inline_workflow_count += 1
                                except Exception as e:
                                    failed_workflow_count += 1
                                    self._emit_log("error", f"Inline workflow {workflow_id} has invalid format and cannot be loaded")
                                    self._emit_log("error", f"Validation error: {e}")
                                    self._emit_log("error", f"Please re-export your configuration to update inline workflow format")

                    if inline_workflow_count > 0:
                        self._emit_log("info", f"Added {inline_workflow_count} inline workflows to workflow_map")
                    if failed_workflow_count > 0:
                        self._emit_log("error", f"{failed_workflow_count} inline workflows failed validation - re-export config to fix")

                    # Initialize StateExecutor which creates its own ActionExecutor
                    # StateExecutor handles state machine execution (GO_TO_STATE, etc.)
                    # and internally creates ActionExecutor with itself as the state_executor
                    from qontinui.json_executor.state_executor import StateExecutor

                    self.state_executor = StateExecutor(config=self.qontinui_config)
                    self.state_executor.initialize()

                    # Use the StateExecutor's ActionExecutor for action execution
                    # This ensures GO_TO_STATE and other state-based actions work correctly
                    self.action_executor = self.state_executor.action_executor

                    self._emit_log("info", "StateExecutor and ActionExecutor initialized - library will handle all GUI actions")

                    # Apply any existing debug settings to the newly initialized executor
                    if self.debug_settings:
                        self._apply_debug_settings_to_executor()

                    # Initialize unified data architecture services
                    # These depend on state_executor being initialized for state memory access
                    self._initialize_unified_data_services()

                except Exception as e:
                    self._emit_log("error", f"Failed to initialize StateExecutor: {e}")
                    self._emit_log("debug", f"Traceback: {traceback.format_exc()}")
                    # Fall back to None - execution will fail but won't crash
                    self.action_executor = None
                    self.state_executor = None

            config_info = {
                "path": config_path,
                "version": self.config.get("version", "unknown"),
                "name": self.config.get("metadata", {}).get("name", "Unnamed"),
                "workflows": len(self.workflows),
                "images": len(self.images),
                "execution_mode": "sequential",
                "graph_execution_available": False,
                "mock_mode": self.mock_mode,
                "is_mock_mode": self.mock_mode in ("mock", "screenshot"),
                "is_screenshot_mode": self.mock_mode == "screenshot",
            }
            self._emit_event(EventType.CONFIG_LOADED, config_info)
            return True

        except Exception as e:
            self._emit_event(
                EventType.ERROR,
                {
                    "message": "Exception loading configuration",
                    "details": str(e),
                    "traceback": traceback.format_exc(),
                },
            )
            return False

    def _execute_action(self, action_data: dict[str, Any]) -> bool:
        """Execute a single action using ExecutionTree and UnifiedDataCollector.

        The runner's role is to:
        - Track execution hierarchy using ExecutionTree
        - Collect execution data using UnifiedDataCollector
        - Emit action lifecycle events with enriched data
        - Delegate actual execution to the library's ActionExecutor

        Integration with unified data architecture:
        1. Extract hierarchy context (parent_action_id, workflow_id, nesting_level)
        2. Start data collection with collector.start_action()
        3. Execute action via ActionExecutor (which emits events to collector)
        4. Create ActionExecutionRecord with collector.create_record()
        5. Enrich ExecutionNode metadata with record data
        6. Emit tree events with complete execution data

        Args:
            action_data: Dictionary containing action type, id, config, etc.

        Returns:
            True if action succeeded, False otherwise
        """
        # File-based debug logging
        import os
        import tempfile
        from datetime import datetime
        debug_log_path = os.path.join(tempfile.gettempdir(), "qontinui_runner_execute_debug.log")

        try:
            with open(debug_log_path, "a", encoding="utf-8") as f:
                timestamp = datetime.now().strftime("%Y-%m-%d %H:%M:%S.%f")[:-3]
                f.write(f"[{timestamp}] _execute_action() called\n")
                f.write(f"[{timestamp}]   Action data: {json.dumps(action_data, indent=2)}\n")
        except Exception:
            pass

        action_type = action_data.get("type")
        action_id = action_data.get("id", f"action-{action_type}-{id(action_data)}")
        config = action_data.get("config", {})

        # Get action definition from registry
        action_def = get_action_definition(action_type)

        # Extract hierarchy context from execution tree for data collection
        # This provides parent_action_id, workflow_id, and nesting_level
        import sys
        print(f"[LEVEL_DEBUG] Before _get_hierarchy_context for action {action_id} type={action_type}", file=sys.stderr, flush=True)
        print(f"[LEVEL_DEBUG]   execution_tree.current = {self.execution_tree.current.id if self.execution_tree.current else 'None'}", file=sys.stderr, flush=True)
        print(f"[LEVEL_DEBUG]   execution_tree.current.get_depth() = {self.execution_tree.current.get_depth() if self.execution_tree.current else 'N/A'}", file=sys.stderr, flush=True)
        hierarchy_context = self._get_hierarchy_context()
        print(f"[LEVEL_DEBUG]   hierarchy_context = {hierarchy_context}", file=sys.stderr, flush=True)

        # Start data collection in UnifiedDataCollector
        # Captures "before" state snapshot and initializes buffer
        if self.unified_data_collector:
            self.unified_data_collector.start_action(
                action_id=action_id,
                parent_action_id=hierarchy_context["parent_action_id"],
                workflow_id=hierarchy_context["workflow_id"],
                nesting_level=hierarchy_context["nesting_level"]
            )

        # Start action in execution tree with metadata
        node = self.execution_tree.start_action(
            action_id,
            action_type,
            metadata={
                "config": config,
                "is_expandable": action_def.is_expandable,
                "action_definition": action_def.to_dict()
            }
        )
        self._emit_tree_event("action_started", node)

        # Capture start time for record creation
        start_time = time.time()
        success = False
        error_message = None

        try:
            self._emit_log("info", f"Executing action: {action_type}")

            # Handle missing action executor
            if not QONTINUI_AVAILABLE or not self.action_executor:
                self._emit_log("warning", f"Simulating action: {action_type} (ActionExecutor not available)")
                time.sleep(0.5)  # Simulate action delay
                success = True
                self.execution_tree.end_action(action_id, success=True)
                self._emit_tree_event("action_completed", node)
                return True

            # Convert action_data to Action object for library
            from qontinui.config.schema import Action

            action = Action(
                id=action_id,
                type=action_type,
                config=config,
                base=action_data.get("base")  # Include base settings (pauseBeforeBegin, pauseAfterEnd, etc.)
            )

            # Handle RUN_WORKFLOW specially - execute workflow via runner's _execute_workflow
            # This ensures each child action goes through _execute_action and emits tree events
            if action_type == "RUN_WORKFLOW":
                workflow_id = config.get("workflowId")
                if workflow_id:
                    # Execute workflow via runner (not library) so child actions emit tree events
                    # _execute_workflow will create the workflow node and execute each action
                    success = self._execute_workflow(workflow_id)
                else:
                    # No workflow ID - fail
                    self._emit_log("error", "RUN_WORKFLOW action missing workflowId")
                    success = False
            else:
                # Normal action delegation
                # During execution, library events are captured by UnifiedDataCollector
                success = self.action_executor.execute_action(action)

                # DEBUG: Log success value immediately after library returns
                import os, tempfile
                from datetime import datetime
                debug_log = os.path.join(tempfile.gettempdir(), "qontinui_action_success_trace.log")
                try:
                    with open(debug_log, "a", encoding="utf-8") as f:
                        ts = datetime.now().strftime("%Y-%m-%d %H:%M:%S.%f")[:-3]
                        f.write(f"[{ts}] LIBRARY RETURNED: action={action_type} id={action_id} success={success} type={type(success)}\n")
                except Exception:
                    pass

            # Sync last_find_location from library if available
            if hasattr(self.action_executor, 'last_find_location') and self.action_executor.last_find_location:
                from qontinui import Location
                loc = self.action_executor.last_find_location
                self._last_find_location = Location(loc[0], loc[1])
                self._emit_log("debug", f"Synced last_find_location from library: {self._last_find_location}")

            # Handle GO_TO_STATE actions specially
            # Record transition data in collector
            if action_type == "GO_TO_STATE" and self.unified_data_collector:
                # Extract transition information from config
                target_state = config.get("targetState", "unknown")
                # Get current state from state_executor (this is the "from" state)
                from_state = self.state_executor.current_state if self.state_executor else None

                transition_data = {
                    "from_state": from_state,
                    "to_state": target_state,
                    "transition_type": "navigation",
                    "success": success
                }
                self.unified_data_collector.record_transition(transition_data)

            # Handle RUN_WORKFLOW actions specially
            # Add workflow name to metadata so it can be displayed in frontend
            if action_type == "RUN_WORKFLOW" and self.unified_data_collector:
                workflow_id = config.get("workflowId")
                if workflow_id:
                    # Try to get workflow name from registry or local workflows
                    workflow_name = workflow_id  # Default to ID
                    if workflow_id in self.workflows:
                        workflow_data = self.workflows[workflow_id]
                        if isinstance(workflow_data, dict):
                            workflow_name = workflow_data.get("name", workflow_id)

                    # Record workflow metadata
                    # This will be picked up by _format_runtime_for_display in ActionExecutionRecord
                    self.unified_data_collector.record_workflow_execution(
                        workflow_name=workflow_name,
                        workflow_status="success" if success else "failed"
                    )

        except Exception as e:
            # DEBUG: Log exception details
            import os, tempfile
            from datetime import datetime
            debug_log = os.path.join(tempfile.gettempdir(), "qontinui_action_success_trace.log")
            try:
                with open(debug_log, "a", encoding="utf-8") as f:
                    ts = datetime.now().strftime("%Y-%m-%d %H:%M:%S.%f")[:-3]
                    f.write(f"[{ts}] EXCEPTION CAUGHT: action={action_type} id={action_id} exception={e}\n")
                    import traceback
                    f.write(f"[{ts}] TRACEBACK:\n")
                    traceback.print_exc(file=f)
            except Exception:
                pass

            success = False
            error_message = str(e)
            self._emit_log("error", f"Action failed with exception: {e}")

        finally:
            # Capture end time
            end_time = time.time()

            # Debug: Only use stderr for non-JSON debug output
            import sys
            print(f"[FINALLY_BLOCK] Action {action_id} type={action_type} entering finally block", file=sys.stderr, flush=True)

            # Enrich FIND action config with human-readable image names
            print(f"[ENRICHMENT] Checking FIND action enrichment: action_type={action_type}", file=sys.stderr, flush=True)
            if action_type == "FIND":
                print(f"[ENRICHMENT] FIND action detected, config keys: {config.keys() if isinstance(config, dict) else 'not a dict'}", file=sys.stderr, flush=True)
                print(f"[ENRICHMENT] FIND action detected, config: {config}", file=sys.stderr, flush=True)
                config_copy = None

                # Check for imageId at root level
                if "imageId" in config:
                    image_id = config.get("imageId")
                    image_name = self._get_image_name(image_id)
                    if image_name:
                        config_copy = config.copy() if config_copy is None else config_copy
                        config_copy["imageName"] = image_name
                        self._emit_log("debug", f"Enriched FIND action (root imageId) with image name: {image_name}")

                # Check for imageId nested in target object
                target = config.get("target")
                if isinstance(target, dict):
                    if "imageId" in target:
                        image_id = target.get("imageId")
                        image_name = self._get_image_name(image_id)
                        if image_name:
                            config_copy = config.copy() if config_copy is None else config_copy
                            target_copy = config_copy.get("target", {}).copy() if isinstance(config_copy.get("target"), dict) else {}
                            target_copy["imageName"] = image_name
                            config_copy["target"] = target_copy
                            # Also add at root for easier frontend access
                            config_copy["imageName"] = image_name
                            self._emit_log("debug", f"Enriched FIND action (target.imageId) with image name: {image_name}")

                    # Handle multiple images in target
                    if "imageIds" in target:
                        image_ids = target.get("imageIds", [])
                        image_names = [self._get_image_name(img_id) for img_id in image_ids]
                        image_names = [name for name in image_names if name]  # Filter None
                        if image_names:
                            config_copy = config.copy() if config_copy is None else config_copy
                            target_copy = config_copy.get("target", {}).copy() if isinstance(config_copy.get("target"), dict) else {}
                            target_copy["imageNames"] = image_names
                            config_copy["target"] = target_copy
                            # Also add at root for easier frontend access
                            config_copy["imageNames"] = image_names
                            config_copy["imageName"] = ", ".join(image_names)
                            self._emit_log("debug", f"Enriched FIND action (target.imageIds) with {len(image_names)} image names")

                # Handle multiple images at root level
                if "imageIds" in config:
                    image_ids = config.get("imageIds", [])
                    image_names = [self._get_image_name(img_id) for img_id in image_ids]
                    image_names = [name for name in image_names if name]  # Filter None
                    if image_names:
                        config_copy = config.copy() if config_copy is None else config_copy
                        config_copy["imageNames"] = image_names
                        config_copy["imageName"] = ", ".join(image_names)
                        self._emit_log("debug", f"Enriched FIND action (root imageIds) with {len(image_names)} image names")

                # Use enriched config if we made changes
                if config_copy:
                    config = config_copy
                    # Also update the node metadata with enriched config
                    node.metadata["config"] = config
                    print(f"[ENRICHMENT] Updated node metadata with enriched config", file=sys.stderr, flush=True)

            # Create ActionExecutionRecord with UnifiedDataCollector
            # This captures "after" state snapshot, extracts buffered data, and creates immutable record
            if self.unified_data_collector:
                record = self.unified_data_collector.create_record(
                    action_type=action_type,
                    config=config,
                    start_time=start_time,
                    end_time=end_time,
                    success=success,
                    error_message=error_message,
                    retry_count=0,  # TODO: Track actual retry count from ActionExecutor
                    metadata={
                        "action_definition": action_def.to_dict()
                    }
                )

                # Enrich ExecutionNode metadata with record data
                # This allows tree events to include complete execution information
                node.metadata.update({
                    "execution_record": record.to_tree_event_data()
                })

            # End action in execution tree
            self.execution_tree.end_action(action_id, success=success, error=error_message)

            # DEBUG: Log final success value before emitting to frontend
            import os, tempfile
            from datetime import datetime
            debug_log = os.path.join(tempfile.gettempdir(), "qontinui_action_success_trace.log")
            try:
                with open(debug_log, "a", encoding="utf-8") as f:
                    ts = datetime.now().strftime("%Y-%m-%d %H:%M:%S.%f")[:-3]
                    event_type = "action_completed" if success else "action_failed"
                    f.write(f"[{ts}] EMITTING TO FRONTEND: action={action_type} id={action_id} success={success} event={event_type}\n")
                    f.write(f"[{ts}]   error_message={error_message}\n")
                    f.write(f"[{ts}]   node.metadata keys={list(node.metadata.keys())}\n")
                    if 'execution_record' in node.metadata:
                        f.write(f"[{ts}]   execution_record.success={node.metadata['execution_record'].get('success', 'N/A')}\n")
                    f.write("\n")
            except Exception:
                pass

            # Emit tree event with enriched metadata
            self._emit_tree_event("action_completed" if success else "action_failed", node)

            return success

    def _execute_workflow(self, workflow_id: str) -> bool:
        """Execute a workflow using manual execution (graph execution not available)."""
        # Note: Graph execution not available - json_executor modules don't exist
        return self._execute_workflow_manual(workflow_id)

    def _execute_workflow_manual(self, workflow_id: str) -> bool:
        """Manual workflow execution using ExecutionTree for tracking."""
        # Get workflow data (actions and name)
        workflow_data = None
        workflow_name = workflow_id  # Default to ID if name not found
        is_inline = False  # Track if this is an inline transition workflow

        # Check if this workflow is being called from within an action (not top-level)
        # If we're inside an action node, this is an inline workflow (called by GO_TO_STATE or similar)
        current_node = self.execution_tree.current
        if current_node and current_node.node_type == "action":
            is_inline = True
            self._emit_log("debug", f"Workflow {workflow_id} called from action {current_node.name}, marking as inline")

        # Try local workflows first
        if workflow_id in self.workflows:
            workflow_data = self.workflows[workflow_id]
            if isinstance(workflow_data, dict):
                actions = workflow_data.get("actions", [])
                workflow_name = workflow_data.get("name", workflow_id)
            else:
                # Legacy format: just actions array
                actions = workflow_data
        # Fallback to registry for inline workflows registered by transition loader
        elif QONTINUI_AVAILABLE:
            actions = registry.get_workflow(workflow_id)
            if actions is None:
                self._emit_log("error", f"Workflow {workflow_id} not found in local workflows or registry")
                return False
            # Try to get name from registry
            workflow_name = registry.get_workflow_name(workflow_id) if hasattr(registry, 'get_workflow_name') else workflow_id
            # This is an inline workflow from a transition - mark it
            is_inline = True
            # Cache it locally for future use
            self.workflows[workflow_id] = {
                "actions": actions,
                "name": workflow_name
            }
            self._emit_log("debug", f"Loaded inline workflow {workflow_id} from registry")
        else:
            self._emit_log("error", f"Workflow {workflow_id} not found")
            return False

        # Start workflow in execution tree (mark inline workflows appropriately)
        node = self.execution_tree.start_workflow(workflow_id, workflow_name, is_inline=is_inline)
        self._emit_log("debug", f"[TREE] Starting workflow node: {workflow_id} (inline={is_inline})")
        self._emit_tree_event("workflow_started", node)
        self._emit_log("debug", f"[TREE] Emitted workflow_started tree event")

        success = True

        # File-based debug logging
        import os
        import tempfile
        from datetime import datetime
        workflow_debug_log = os.path.join(tempfile.gettempdir(), "qontinui_workflow_execution_debug.log")

        try:
            with open(workflow_debug_log, "a", encoding="utf-8") as f:
                timestamp = datetime.now().strftime("%Y-%m-%d %H:%M:%S.%f")[:-3]
                f.write(f"[{timestamp}] Executing workflow {workflow_id} with {len(actions)} actions\n")
        except Exception:
            pass

        try:
            for idx, action in enumerate(actions):
                # Log each action
                try:
                    with open(workflow_debug_log, "a", encoding="utf-8") as f:
                        timestamp = datetime.now().strftime("%Y-%m-%d %H:%M:%S.%f")[:-3]
                        f.write(f"[{timestamp}] Action {idx+1}/{len(actions)}: {action.get('type')} (ID: {action.get('id')})\n")
                except Exception:
                    pass

                if not self.is_running:
                    break

                # Execute action and track success, but always continue
                # Model-based GUI automation should be resilient - never stop on action failure
                action_success = self._execute_action(action)

                # Log result
                try:
                    with open(workflow_debug_log, "a", encoding="utf-8") as f:
                        timestamp = datetime.now().strftime("%Y-%m-%d %H:%M:%S.%f")[:-3]
                        f.write(f"[{timestamp}] Action result: {action_success}\n")
                except Exception:
                    pass

                if not action_success:
                    success = False
                    self._emit_log("debug", f"Action failed, continuing workflow (model-based automation principle)")

                # Small delay between actions
                time.sleep(0.5)

            # End workflow with success status
            self.execution_tree.end_workflow(workflow_id, success=success)
            self._emit_tree_event("workflow_completed" if success else "workflow_failed", node)

        except Exception as e:
            # End workflow with failure status
            self.execution_tree.end_workflow(workflow_id, success=False, error=str(e))
            self._emit_tree_event("workflow_failed", node, {"error": str(e)})
            success = False

        return success

    def execute_workflow(self, workflow_id: str, transition_context: dict = None) -> dict:
        """Execute a workflow and return result in navigation API expected format.

        This method is called by the navigation system when executing transitions.
        It wraps _execute_workflow() to provide the expected return format.

        Args:
            workflow_id: ID of workflow to execute
            transition_context: Optional dict with transition metadata:
                - transition_id: Unique ID for the transition
                - transition_name: Display name (e.g., "outgoing (processing, inventory)")
                - transition_type: "outgoing" or "incoming"
                - target_states: List of state names or IDs being activated
                - source_state: Source state name (for display)

        Returns:
            dict with 'success' key: {'success': True/False}
        """
        try:
            # If transition context provided, create a transition node
            if transition_context:
                transition_id = transition_context.get('transition_id', f"transition-{time.time()}")
                transition_name = transition_context.get('transition_name', 'Transition')

                # Start transition node
                trans_node = self.execution_tree.start_transition(
                    transition_id,
                    transition_name,
                    metadata={
                        'transition_type': transition_context.get('transition_type'),
                        'target_states': transition_context.get('target_states', []),
                        'source_state': transition_context.get('source_state'),
                        'is_expandable': True  # Transitions are always expandable
                    }
                )
                self._emit_tree_event("transition_started", trans_node)

                try:
                    # Execute workflow under this transition
                    success = self._execute_workflow(workflow_id)

                    # End transition node
                    self.execution_tree.end_transition(transition_id, success=success)
                    self._emit_tree_event("transition_completed" if success else "transition_failed", trans_node)

                    return {'success': success}
                except Exception as e:
                    # End transition with failure
                    self.execution_tree.end_transition(transition_id, success=False, error=str(e))
                    self._emit_tree_event("transition_failed", trans_node, {"error": str(e)})
                    raise
            else:
                # No transition context - execute workflow normally
                success = self._execute_workflow(workflow_id)
                return {'success': success}
        except Exception as e:
            self._emit_log("error", f"Workflow execution failed: {e}")
            return {'success': False, 'error': str(e)}

    def _run_workflow(self, workflow_id: str):
        """Run a specific workflow directly."""
        try:
            self._emit_log("info", f"Thread started - beginning workflow execution: {workflow_id}")
            self._emit_log("debug", f"Workflow exists: {workflow_id in self.workflows}")
            self._emit_log("debug", f"Available workflows: {list(self.workflows.keys())}")

            # Reset navigation state to initial conditions before each run
            # This ensures the automation starts from the same state every time
            if QONTINUI_AVAILABLE:
                try:
                    reset_success = navigation_api.reset_to_initial_state()
                    if reset_success:
                        self._emit_log("info", "Navigation state reset to initial conditions")
                    else:
                        self._emit_log("warning", "Failed to reset navigation state - continuing anyway")
                except Exception as e:
                    self._emit_log("warning", f"Error resetting navigation state: {e}")

            success = self._execute_workflow(workflow_id)

            self._emit_event(
                EventType.EXECUTION_COMPLETED,
                {
                    "success": success,
                    "workflow_id": workflow_id,
                },
            )

            # End WebSocket session on success
            if self.ws_enabled and self.ws_client:
                self._ws_end_session(status="completed")

        except Exception as e:
            self._emit_log("error", f"Exception in _run_workflow: {e}")
            self._emit_event(
                EventType.ERROR,
                {
                    "message": "Workflow execution failed",
                    "details": str(e),
                    "traceback": traceback.format_exc(),
                },
            )

            # End WebSocket session on failure
            if self.ws_enabled and self.ws_client:
                self._ws_end_session(status="failed", error=str(e))

        finally:
            self._emit_log("debug", "Thread completing, setting is_running=False")
            self.is_running = False

    def start_execution(self, workflow_id: str) -> bool:
        """Start workflow execution.

        Args:
            workflow_id: Workflow ID to execute
        """
        if not self.config:
            self._emit_event(EventType.ERROR, {"message": "No configuration loaded"})
            return False

        if not QONTINUI_AVAILABLE:
            self._emit_event(
                EventType.ERROR, {"message": "Cannot execute without Qontinui library"}
            )
            return False

        if self.is_running:
            self._emit_event(EventType.ERROR, {"message": "Execution already in progress"})
            return False

        if not workflow_id:
            self._emit_log("error", "Workflow ID is required")
            return False

        try:
            self.is_running = True

            # Start WebSocket session if enabled
            if self.ws_enabled and self.ws_client:
                # Create config snapshot for session
                config_snapshot = {
                    "workflow_id": workflow_id,
                    "workflows": [w for w in (self.config.get("workflows", []) if isinstance(self.config, dict) else [])],
                }
                self._ws_start_session(config_snapshot)

            self._emit_event(
                EventType.EXECUTION_STARTED, {"workflow_id": workflow_id}
            )

            # Run workflow in separate thread
            execution_thread = threading.Thread(target=self._run_workflow, args=(workflow_id,))
            execution_thread.daemon = True
            execution_thread.start()

            return True

        except Exception as e:
            self._emit_event(
                EventType.ERROR,
                {
                    "message": "Failed to start execution",
                    "details": str(e),
                    "traceback": traceback.format_exc(),
                },
            )
            self.is_running = False
            return False

    def stop_execution(self):
        """Stop the current execution."""
        if self.is_running:
            self._emit_log("info", "Stopping execution...")
            self.is_running = False
            self._emit_event(
                EventType.EXECUTION_COMPLETED, {"success": False, "reason": "User stopped"}
            )

    def handle_command(self, command: dict[str, Any]) -> dict[str, Any]:
        """Handle command from Tauri."""
        cmd_type = command.get("command")
        params = command.get("params", {})

        # Log command receipt at INFO level so it appears in backend logs
        self._emit_log("info", f"handle_command: received '{cmd_type}'")

        if cmd_type == "load":
            config_path = params.get("config_path")
            success = self.load_configuration(config_path)
            return {"success": success}

        elif cmd_type == "start":
            # Get workflow_id from params
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
            # Update debug settings from frontend
            settings = params.get("settings", {})
            self.set_debug_settings(settings)
            return {"success": True}

        elif cmd_type == "update_capture_settings":
            # Update screenshot capture tool settings
            settings = params.get("settings", {})
            return self.update_capture_settings(settings)

        elif cmd_type == "start_manual_capture":
            # Manually start manual click capture
            if not self.capture_tool_service.is_enabled():
                return {"success": False, "error": "Capture service not enabled"}
            if self.manual_click_listener.is_running():
                return {"success": False, "error": "Manual capture already running"}
            success = self.manual_click_listener.start()
            return {"success": success}

        elif cmd_type == "stop_manual_capture":
            # Manually stop manual click capture
            self.manual_click_listener.stop()
            return {"success": True}

        elif cmd_type == "manual_capture_status":
            # Check manual click capture status
            return {
                "success": True,
                "is_running": self.manual_click_listener.is_running(),
                "capture_enabled": self.capture_tool_service.is_enabled()
            }

        elif cmd_type == "ws_configure":
            # Configure WebSocket from params
            config_dict = params.get("config", {})
            try:
                self.ws_config = WebSocketConfig.from_dict(config_dict)
                return {"success": True, "message": "WebSocket configured"}
            except Exception as e:
                return {"success": False, "error": str(e)}

        elif cmd_type == "ws_connect":
            # Connect to WebSocket server
            success = self._ws_connect()
            return {"success": success}

        elif cmd_type == "ws_disconnect":
            # Disconnect from WebSocket server
            self._ws_disconnect()
            return {"success": True}

        elif cmd_type == "ws_start_session":
            # Start WebSocket session
            config_snapshot = params.get("config_snapshot")
            success = self._ws_start_session(config_snapshot)
            return {"success": success}

        elif cmd_type == "ws_end_session":
            # End WebSocket session
            status = params.get("status", "completed")
            error = params.get("error")
            success = self._ws_end_session(status, error)
            return {"success": success}

        elif cmd_type == "ws_status":
            # Get WebSocket status
            stats = self.ws_client.get_stats() if self.ws_client else {}
            return {
                "success": True,
                "enabled": self.ws_enabled,
                "connected": self.ws_enabled and self.ws_client is not None,
                "stats": stats,
                "config": self.ws_config.to_dict() if self.ws_config else None,
            }

        elif cmd_type == "ping":
            # Health check ping - send pong message directly to stdout
            pong_message = {
                "type": "pong",
                "timestamp": time.time()
            }
            print(json.dumps(pong_message), flush=True)
            # Also return success response
            return {"success": True}

        elif cmd_type == "execute_transition":
            return execute_transition(params.get("transition_id"))

        elif cmd_type == "navigate_to_state":
            return navigate_to_state(params.get("state_id"))

        elif cmd_type == "navigate_to_multiple_states":
            return navigate_to_multiple_states(params.get("state_ids", []))

        elif cmd_type == "get_active_states":
            return get_active_states()

        elif cmd_type == "get_available_transitions":
            return get_available_transitions()

        else:
            return {"success": False, "error": f"Unknown command: {cmd_type}"}

    def __del__(self):
        """Clean up temp directory on exit."""
        # Stop manual click listener
        if hasattr(self, 'manual_click_listener'):
            try:
                self.manual_click_listener.cleanup()
            except Exception:
                pass

        # Clean up temp directory
        if self.temp_dir and os.path.exists(self.temp_dir):
            import contextlib
            import shutil

            with contextlib.suppress(Exception):
                shutil.rmtree(self.temp_dir)


# ============================================================================
# Global Orchestrator Management
# ============================================================================

_orchestrator: Optional[RunnerOrchestrator] = None


def initialize_orchestrator(config_path: str) -> None:
    """Initialize the global orchestrator instance.

    Args:
        config_path: Path to JSON configuration file

    Raises:
        RuntimeError: If initialization fails
    """
    global _orchestrator
    _orchestrator = RunnerOrchestrator(config_path)
    logger.info(f"Orchestrator initialized with config: {config_path}")


def get_orchestrator() -> RunnerOrchestrator:
    """Get the global orchestrator instance.

    Returns:
        RunnerOrchestrator instance

    Raises:
        RuntimeError: If orchestrator not initialized
    """
    if _orchestrator is None:
        raise RuntimeError("Orchestrator not initialized. Call initialize_orchestrator() first.")
    return _orchestrator


# ============================================================================
# Tauri Command Bindings
# ============================================================================

def execute_transition(transition_id: str) -> dict:
    """Execute transition via library.

    Args:
        transition_id: ID of transition to execute

    Returns:
        Dict with success status and details
    """
    orchestrator = get_orchestrator()
    return orchestrator.execute_transition(transition_id)


def navigate_to_state(state_id: str) -> dict:
    """Navigate to state via library.

    Args:
        state_id: ID of target state

    Returns:
        Dict with success status and details
    """
    orchestrator = get_orchestrator()
    return orchestrator.navigate_to_state(state_id)


def navigate_to_multiple_states(state_ids: List[str]) -> dict:
    """Navigate to multiple states via library.

    Args:
        state_ids: List of target state IDs

    Returns:
        Dict with success status and details
    """
    orchestrator = get_orchestrator()
    return orchestrator.navigate_to_multiple_states(state_ids)


def get_active_states() -> dict:
    """Get active states from library.

    Returns:
        Dict with active state information
    """
    orchestrator = get_orchestrator()
    return orchestrator.get_active_states()


def get_available_transitions() -> dict:
    """Get available transitions from library.

    Returns:
        Dict with available transition information
    """
    orchestrator = get_orchestrator()
    return orchestrator.get_available_transitions()


# ============================================================================
# Main Entry Point
# ============================================================================

def main():
    """Main entry point for the Qontinui executor."""
    executor = QontinuiExecutor()

    # Emit startup log via JSON event (will appear in backend logs)
    executor._emit_log("info", "Python executor main loop started, waiting for commands")

    # Read commands from stdin
    for line in sys.stdin:
        try:
            # Only log non-ping commands to avoid spam
            command = json.loads(line.strip())
            cmd_name = command.get('command', 'unknown')

            if cmd_name != 'ping':
                executor._emit_log("info", f"Received command: {cmd_name}")

            if command.get("type") == "command":
                response = executor.handle_command(command)
                response["id"] = command.get("id")
                response["type"] = "response"
                # Use lock to prevent concurrent threads from interleaving JSON output
                with executor._output_lock:
                    sys.stdout.write(json.dumps(response) + "\n")
                    sys.stdout.flush()

                if cmd_name != 'ping':
                    executor._emit_log("info", f"Command {cmd_name} completed successfully")

        except json.JSONDecodeError as e:
            executor._emit_log("error", f"JSON decode error: {e}")
            executor._emit_event(
                EventType.ERROR, {"message": "Invalid JSON command", "details": str(e)}
            )
        except Exception as e:
            executor._emit_log("error", f"Exception in main loop: {e}\n{traceback.format_exc()}")
            executor._emit_event(
                EventType.ERROR,
                {
                    "message": "Command execution failed",
                    "details": traceback.format_exc(),
                },
            )


if __name__ == "__main__":
    main()
