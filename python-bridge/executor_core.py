"""
Executor Core Module

Single Responsibility: Core executor initialization and configuration loading
- Load and parse configuration files
- Initialize qontinui library components (ActionExecutor, StateExecutor)
- Register images and workflows
- Initialize navigation system
- Configure execution mode (real/mock/screenshot)
- Initialize unified data services
- Coordinate all modules
"""

import base64
import json
import logging
import os
import tempfile
import traceback
from typing import Any

logger = logging.getLogger(__name__)

try:
    from qontinui import Image, navigation_api, registry
    from qontinui.config import disable_mock_mode, enable_mock_mode, get_settings
    from qontinui.json_executor.config_parser import ConfigParser
    from qontinui.json_executor.state_executor import StateExecutor
    from qontinui_schemas.config.models import Workflow, WorkflowVisibility

    QONTINUI_AVAILABLE = True
except ImportError:
    QONTINUI_AVAILABLE = False
    logger.debug("Qontinui library not available")


class ExecutorCore:
    """
    Core executor for configuration loading and initialization.

    Responsibilities:
    - Load JSON configuration
    - Process and register images
    - Process and register workflows
    - Initialize navigation system
    - Initialize ActionExecutor and StateExecutor
    - Configure execution mode
    - Initialize data collection services
    """

    def __init__(self, emit_log_fn, emit_event_fn):
        """
        Initialize executor core.

        Args:
            emit_log_fn: Function to emit log messages (level, message)
            emit_event_fn: Function to emit events (event_type, data)
        """
        self.emit_log = emit_log_fn
        self.emit_event = emit_event_fn

        # Configuration state
        self.config = None
        self.qontinui_config = None
        self.workflows = {}
        self.images = {}
        self.temp_dir = None

        # Execution mode
        self.mock_mode = "real"
        self.screenshot_dir = None
        self.use_graph_execution = False

        # Library components
        self.settings = None
        self.action_executor = None
        self.state_executor = None

        # Debug settings
        self.debug_settings = {"enable_image_debug": False, "top_matches_count": 5}

        # Initialize framework settings if available
        if QONTINUI_AVAILABLE:
            self.settings = get_settings()

    def load_configuration(self, config_path: str) -> bool:
        """
        Load configuration from file and set up Qontinui components.

        Args:
            config_path: Path to JSON configuration file

        Returns:
            True if configuration loaded successfully, False otherwise
        """
        try:
            self.emit_log("info", f"Loading configuration from: {config_path}")

            # Load JSON config
            with open(config_path) as f:
                self.config = json.load(f)

            if not QONTINUI_AVAILABLE:
                self.emit_log(
                    "warning",
                    "Qontinui library not available - config loaded but execution will not work",
                )
                return True

            # Create temp directory for images
            self.temp_dir = tempfile.mkdtemp(prefix="qontinui_")

            # Process images
            self._process_images()

            # Process state images
            self._process_state_images()

            # Process state regions and locations (for monitor association)
            self._process_state_regions_and_locations()

            # Configure execution mode
            self._configure_execution_mode()

            # Process workflows
            self._process_workflows()

            # Initialize navigation system
            self._initialize_navigation()

            # Initialize StateExecutor and ActionExecutor
            self._initialize_executors()

            # Emit config loaded event
            from event_manager import EventType

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
            self.emit_event(EventType.CONFIG_LOADED, config_info)
            return True

        except Exception as e:
            from event_manager import EventType

            self.emit_event(
                EventType.ERROR,
                {
                    "message": "Exception loading configuration",
                    "details": str(e),
                    "traceback": traceback.format_exc(),
                },
            )
            return False

    def _process_images(self):
        """Process images - save to temp files and register in library."""
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

                # Create Qontinui Image object
                image_obj = Image.from_file(img_path)

                # Validate that the image loaded successfully
                if image_obj.is_empty():
                    self.emit_log("error", f"Image {img_id} failed to load - PIL image is empty")
                    self.emit_log("error", f"Check if base64 data is valid for image: {img_name}")
                else:
                    self.emit_log(
                        "debug",
                        f"Loaded image: {img_id} -> {img_path} ({image_obj.width}x{image_obj.height})",
                    )

                self.images[img_id] = image_obj
                # Register image in library's registry
                registry.register_image(img_id, image_obj, file_path=img_path, name=img_name)
            except Exception as e:
                self.emit_log("error", f"Failed to load image {img_id}: {e}")

    def _process_state_images(self):
        """Load state images - map state image IDs to their underlying image objects.

        For StateImages with multiple patterns, ALL pattern images are registered
        in the registry, and a mapping from StateImage ID to pattern image IDs
        is stored to support multi-pattern find operations.
        """
        states = self.config.get("states", [])
        self.emit_log("debug", f"Loading state images from {len(states)} states")

        for state in states:
            state_name = state.get("name", "unknown")
            state_images = state.get("stateImages", [])
            self.emit_log("debug", f"State '{state_name}' has {len(state_images)} state images")

            for state_image in state_images:
                state_image_id = state_image.get("id")
                patterns = state_image.get("patterns", [])
                # Get monitors from state image config
                monitors = state_image.get("monitors")

                if patterns and len(patterns) > 0:
                    # Collect all pattern image IDs for this StateImage
                    pattern_image_ids = []

                    for i, pattern in enumerate(patterns):
                        pattern_image_id = pattern.get("imageId") or pattern.get("image")

                        if pattern_image_id and pattern_image_id in self.images:
                            pattern_image_ids.append(pattern_image_id)

                            # For the first pattern, also map state_image_id -> image
                            # (backwards compatibility with code expecting StateImage ID lookup)
                            if i == 0:
                                self.images[state_image_id] = self.images[pattern_image_id]

                                # Register the state image ID in the library's registry
                                underlying_metadata = registry.get_image_metadata(pattern_image_id)
                                if underlying_metadata:
                                    registry.register_image(
                                        state_image_id,
                                        self.images[pattern_image_id],
                                        file_path=underlying_metadata.get("file_path"),
                                        name=underlying_metadata.get("name"),
                                        monitors=monitors,
                                    )
                                else:
                                    registry.register_image(
                                        state_image_id,
                                        self.images[pattern_image_id],
                                        monitors=monitors,
                                    )
                        else:
                            self.emit_log(
                                "warning",
                                f"State image {state_image_id} pattern {i} references missing image {pattern_image_id}",
                            )

                    # Register the StateImage -> pattern image IDs mapping
                    if pattern_image_ids:
                        registry.register_state_image_patterns(state_image_id, pattern_image_ids)
                        self.emit_log(
                            "debug",
                            f"Mapped state image {state_image_id} -> {len(pattern_image_ids)} pattern(s): {pattern_image_ids} (monitors={monitors})",
                        )
                else:
                    self.emit_log("warning", f"State image {state_image_id} has no patterns")

    def _process_state_regions_and_locations(self):
        """Register StateRegions and StateLocations in the library registry.

        This enables actions to reference regions/locations by ID and use their
        configured monitors instead of a global default.
        """
        states = self.config.get("states", [])
        region_count = 0
        location_count = 0

        for state in states:
            state_name = state.get("name", "unknown")

            # Process regions
            regions = state.get("regions", [])
            for region in regions:
                region_id = region.get("id")
                if not region_id:
                    continue

                x = region.get("x", 0)
                y = region.get("y", 0)
                width = region.get("width", 0)
                height = region.get("height", 0)
                monitors = region.get("monitors")
                name = region.get("name", region_id)

                registry.register_region(
                    region_id=region_id,
                    x=x,
                    y=y,
                    width=width,
                    height=height,
                    monitors=monitors,
                    name=name,
                )
                region_count += 1

            # Process locations
            locations = state.get("locations", [])
            for location in locations:
                location_id = location.get("id")
                if not location_id:
                    continue

                x = location.get("x", 0)
                y = location.get("y", 0)
                monitors = location.get("monitors")
                name = location.get("name", location_id)

                registry.register_location(
                    location_id=location_id,
                    x=x,
                    y=y,
                    monitors=monitors,
                    name=name,
                )
                location_count += 1

        self.emit_log(
            "debug",
            f"Registered {region_count} regions and {location_count} locations from {len(states)} states",
        )

    def _configure_execution_mode(self):
        """Configure execution mode (real/mock/screenshot)."""
        execution_settings = self.config.get("settings", {}).get("execution", {})
        exec_mode_str = execution_settings.get("executionMode", "real").lower()
        screenshot_dir = execution_settings.get("screenshotDirectory")

        try:
            self.mock_mode = exec_mode_str
            self.screenshot_dir = screenshot_dir

            # Update FrameworkSettings based on mode
            if exec_mode_str == "mock":
                enable_mock_mode()
                self.emit_log("info", "Mock mode enabled via FrameworkSettings")
            elif exec_mode_str == "screenshot":
                enable_mock_mode()
                if screenshot_dir and self.settings:
                    self.settings.screenshot_path = screenshot_dir
                    self.settings.save_snapshots = True
                self.emit_log("info", f"Screenshot mode enabled, directory: {screenshot_dir}")
            else:  # real mode
                disable_mock_mode()
                self.emit_log("info", "Real execution mode enabled")

        except Exception as e:
            self.emit_log(
                "warning", f"Failed to initialize execution mode: {e}. Defaulting to REAL mode."
            )
            self.mock_mode = "real"
            disable_mock_mode()

        # Check for graph execution setting
        self.use_graph_execution = False
        if execution_settings.get("useGraphExecution", False):
            self.emit_log(
                "warning",
                "Graph execution requested but not available. Using sequential execution.",
            )

    def _process_workflows(self):
        """Process workflows and register in library."""
        workflow_data = self.config.get("workflows", [])
        for workflow in workflow_data:
            workflow_id = workflow.get("id")
            workflow_name = workflow.get("name", workflow_id)
            actions = workflow.get("actions", [])

            # Store both actions and name
            self.workflows[workflow_id] = {"actions": actions, "name": workflow_name}

            # Register workflow in library's registry
            registry.register_workflow(workflow_id, actions, workflow_name)
            self.emit_log("debug", f"Registered workflow: {workflow_name}")

    def _initialize_navigation(self):
        """Initialize navigation system with config."""
        try:
            success = navigation_api.load_configuration(self.config)
            if success:
                self.emit_log("info", "Navigation system initialized with states and transitions")
            else:
                self.emit_log(
                    "error", "Failed to initialize navigation system - check configuration"
                )

        except Exception as e:
            self.emit_log("warning", f"Failed to initialize navigation: {e}")
            self.emit_log("debug", f"Traceback: {traceback.format_exc()}")

    def _initialize_executors(self):
        """Initialize StateExecutor and ActionExecutor."""
        # DEBUG: Log to file
        import os
        import tempfile

        debug_log_path = os.path.join(tempfile.gettempdir(), "qontinui_init_executors_debug.log")
        try:
            with open(debug_log_path, "a") as f:
                from datetime import datetime

                f.write(f"\n=== _INITIALIZE_EXECUTORS DEBUG {datetime.now()} ===\n")
                f.flush()
        except Exception:
            pass

        try:
            # Parse JSON config into QontinuiConfig object
            config_parser = ConfigParser()
            self.qontinui_config = config_parser.parse_config(self.config)
            self.emit_log(
                "info",
                f"Parsed config: {len(self.qontinui_config.workflows)} workflows, {len(self.qontinui_config.states)} states",
            )

            # Sync inline workflows from registry to workflow_map
            self._sync_inline_workflows()

            # Initialize StateExecutor
            self.state_executor = StateExecutor(config=self.qontinui_config)
            self.state_executor.initialize()

            # Use the StateExecutor's ActionExecutor
            self.action_executor = self.state_executor.action_executor

            self.emit_log("info", "StateExecutor and ActionExecutor initialized")

            # Apply debug settings if available
            if self.debug_settings:
                self.apply_debug_settings(self.debug_settings)

        except Exception as e:
            self.emit_log("error", f"Failed to initialize StateExecutor: {e}")
            self.emit_log("debug", f"Traceback: {traceback.format_exc()}")
            # DEBUG: Write error to file
            try:
                with open(debug_log_path, "a") as f:
                    f.write(f"EXCEPTION: {e}\n")
                    f.write(f"TRACEBACK:\n{traceback.format_exc()}\n")
                    f.flush()
            except Exception:
                pass
            self.action_executor = None
            self.state_executor = None

    def _sync_inline_workflows(self):
        """Sync inline workflows from registry to workflow_map."""
        registry_workflow_ids = registry.get_all_workflow_ids()
        inline_workflow_count = 0
        failed_workflow_count = 0

        for workflow_id in registry_workflow_ids:
            if workflow_id not in self.qontinui_config.workflow_map:
                # This is an inline workflow
                workflow_def = registry.get_workflow_definition(workflow_id)
                if workflow_def:
                    workflow_def["visibility"] = WorkflowVisibility.INTERNAL.value
                    try:
                        workflow_obj = Workflow(**workflow_def)
                        self.qontinui_config.workflow_map[workflow_id] = workflow_obj
                        inline_workflow_count += 1
                    except Exception as e:
                        failed_workflow_count += 1
                        self.emit_log("error", f"Inline workflow {workflow_id} has invalid format")
                        self.emit_log("error", f"Validation error: {e}")

        if inline_workflow_count > 0:
            self.emit_log("info", f"Added {inline_workflow_count} inline workflows to workflow_map")
        if failed_workflow_count > 0:
            self.emit_log("error", f"{failed_workflow_count} inline workflows failed validation")

    def apply_debug_settings(self, settings: dict[str, Any]):
        """
        Apply debug settings to the framework.

        Args:
            settings: Dictionary containing debug configuration
        """
        self.debug_settings = settings
        self.emit_log("info", f"Debug settings updated: {settings}")

        if QONTINUI_AVAILABLE and self.action_executor:
            try:
                from qontinui.config.framework_settings import FrameworkSettings

                fw_settings = FrameworkSettings.get_instance()

                # Default to True for debug data collection (allows visual debug image generation)
                enable_debug = settings.get("enable_image_debug", True)
                fw_settings.image_debug.emit_match_details = enable_debug

                self.emit_log("info", f"Applied debug settings: emit_match_details={enable_debug}")
            except Exception as e:
                self.emit_log("error", f"Failed to apply debug settings: {e}")

    def get_temp_dir(self) -> str | None:
        """Get temporary directory for this execution."""
        return self.temp_dir  # type: ignore[no-any-return]

    def cleanup(self):
        """Clean up temporary resources."""
        if self.temp_dir and os.path.exists(self.temp_dir):
            import shutil

            try:
                shutil.rmtree(self.temp_dir)
                self.emit_log("debug", f"Cleaned up temp directory: {self.temp_dir}")
            except Exception as e:
                self.emit_log("warning", f"Failed to clean up temp directory: {e}")
