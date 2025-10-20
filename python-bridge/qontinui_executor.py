#!/usr/bin/env python3
"""
Qontinui executor that integrates with the actual Qontinui library.
"""

import base64
import json
import os
import sys
import tempfile
import threading
import time
import traceback
from enum import Enum
from pathlib import Path
from typing import Any

# Add parent directory to path to import qontinui
sys.path.insert(0, str(Path(__file__).parent.parent.parent))

try:
    from qontinui import Find, FluentActions, Image, Location, QontinuiStateManager, State
    from qontinui.config.execution_mode import ExecutionModeConfig, MockMode, set_execution_mode
    from qontinui.json_executor.action_executor import ActionExecutor
    from qontinui.json_executor.config_parser import ConfigParser
    from qontinui.model.element import Position
    from qontinui.model.state import StateLocation
    from qontinui.wrappers import get_controller

    QONTINUI_AVAILABLE = True
except ImportError:
    QONTINUI_AVAILABLE = False
    print(
        json.dumps(
            {
                "type": "event",
                "event": "error",
                "timestamp": time.time(),
                "sequence": 0,
                "data": {
                    "message": "Qontinui library not available. Please install qontinui package.",
                    "details": "Run: pip install -e /home/jspinak/qontinui_parent_directory/qontinui",
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
    WORKFLOW_STARTED = "workflow_started"  # v2.0.0 terminology
    WORKFLOW_COMPLETED = "workflow_completed"  # v2.0.0 terminology
    PROCESS_STARTED = "process_started"  # Deprecated: use WORKFLOW_STARTED
    PROCESS_COMPLETED = "process_completed"  # Deprecated: use WORKFLOW_COMPLETED
    EXECUTION_COMPLETED = "execution_completed"
    ERROR = "error"
    LOG = "log"
    MATCH_FOUND = "match_found"
    SCREENSHOT_TAKEN = "screenshot_taken"
    IMAGE_RECOGNITION = "image_recognition"
    ACTION_EXECUTION = "action_execution"
    RECORDING_STARTED = "recording_started"
    RECORDING_STOPPED = "recording_stopped"


class QontinuiExecutor:
    """Executor that uses the Qontinui library for real automation."""

    def __init__(self):
        self.config = None
        self.state_manager = None
        self.states = {}
        self.transitions = {}
        self.workflows = {}  # v2.0.0: renamed from processes
        self.processes = self.workflows  # Backward compatibility alias
        self.images = {}
        self.current_state = None
        self.is_running = False
        self._sequence = 0
        self.temp_dir = None
        self.use_graph_execution = False
        self.qontinui_config = None
        self.action_executor = None
        self.execution_mode = None  # ExecutionModeConfig instance
        self.controller = None  # ExecutionModeController instance

        if QONTINUI_AVAILABLE:
            self.state_manager = QontinuiStateManager()
            self.actions = FluentActions()

        self._emit_event(
            EventType.READY,
            {"message": "Qontinui executor initialized", "library_available": QONTINUI_AVAILABLE},
        )

    def _emit_event(self, event_type: EventType, data: dict[str, Any]):
        """Emit event to Tauri through stdout."""
        event = {
            "type": "event",
            "event": event_type.value,
            "timestamp": time.time(),
            "sequence": self._sequence,
            "data": data,
        }
        self._sequence += 1
        print(json.dumps(event), flush=True)

    def _emit_log(self, level: str, message: str):
        """Emit log message."""
        self._emit_event(EventType.LOG, {"level": level, "message": message})

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

            # Get template image
            image_obj = self.images[image_id]
            image_path = getattr(image_obj, "path", None)
            if not image_path:
                return None

            template = cv2.imread(image_path)
            if template is None:
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
        image_path = getattr(image_obj, "path", None) or image_id

        # Try to get template size
        try:
            import cv2

            template = cv2.imread(image_path) if hasattr(cv2, "imread") else None
            template_size = (
                f"{template.shape[1]}x{template.shape[0]}" if template is not None else "unknown"
            )
        except Exception:
            template_size = "unknown"

        # Try to get screenshot size
        screenshot_size = "unknown"
        try:
            from PIL import ImageGrab

            screenshot = ImageGrab.grab()
            screenshot_size = f"{screenshot.width}x{screenshot.height}"
        except Exception:
            pass

        if matches:
            # Get confidence from first match
            first_match = matches[0]
            confidence = getattr(first_match, "score", threshold)
            location = f"({getattr(first_match, 'x', 0)}, {getattr(first_match, 'y', 0)})"

            # Emit event for successful match
            event_data = {
                "image_path": image_path,
                "template_size": template_size,
                "screenshot_size": screenshot_size,
                "threshold": threshold,
                "confidence": confidence,
                "found": True,
                "location": location,
                "gap": threshold - confidence if confidence < threshold else 0,
                "percent_off": (
                    ((threshold - confidence) / threshold * 100) if confidence < threshold else 0
                ),
            }
            self._emit_log(
                "debug",
                f"Emitting IMAGE_RECOGNITION event (FOUND): {image_path}, confidence: {confidence}",
            )
            self._emit_event(EventType.IMAGE_RECOGNITION, event_data)
        else:
            # Build event data for no match found
            event_data = {
                "image_path": image_path,
                "template_size": template_size,
                "screenshot_size": screenshot_size,
                "threshold": threshold,
                "confidence": 0.0,
                "found": False,
            }

            # Add best match information if available
            if best_match_info:
                best_confidence = best_match_info.get("confidence", 0.0)
                best_x = best_match_info.get("x", 0)
                best_y = best_match_info.get("y", 0)

                event_data["confidence"] = best_confidence
                event_data["best_match_location"] = f"({best_x}, {best_y})"
                event_data["gap"] = threshold - best_confidence
                event_data["percent_off"] = (
                    ((threshold - best_confidence) / threshold * 100) if threshold > 0 else 0
                )

            # Emit event
            self._emit_log(
                "debug",
                f"Emitting IMAGE_RECOGNITION event (NOT FOUND): {image_path}, best_match: {best_match_info is not None}",
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

            # Process images - save to temp files
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
                        self.images[img_id] = Image(img_path)
                    else:
                        # Store path for testing purposes
                        self.images[img_id] = {"path": img_path}
                    self._emit_log("debug", f"Loaded image: {img_id} -> {img_path}")
                except Exception as e:
                    self._emit_log("error", f"Failed to load image {img_id}: {e}")

            # Process states (only if Qontinui is available)
            if QONTINUI_AVAILABLE:
                for state_data in self.config.get("states", []):
                    state_id = state_data.get("id")
                    state = State(
                        name=state_data.get("name", state_id),
                        description=state_data.get("description", ""),
                    )

                    # Add identifying images to state
                    for img_info in state_data.get("identifyingImages", []):
                        # Handle both string IDs and object format
                        if isinstance(img_info, str):
                            img_id = img_info
                        else:
                            img_id = img_info.get("image") or img_info.get("imageId")

                        if img_id and img_id in self.images:
                            state.add_image(self.images[img_id])

                    # Process StateLocations for this state
                    for loc_data in state_data.get("locations", []):
                        try:
                            # Create base Location object
                            location = Location(
                                x=loc_data.get("x", 0),
                                y=loc_data.get("y", 0),
                                name=loc_data.get("name"),
                                offset_x=loc_data.get("offsetX", 0),
                                offset_y=loc_data.get("offsetY", 0),
                                fixed=loc_data.get("fixed", True),
                            )

                            # Handle reference image for relative positioning
                            ref_img_id = loc_data.get("referenceImageId")
                            if ref_img_id and ref_img_id in self.images:
                                location.reference_image_id = ref_img_id

                                # Handle position within the image
                                pos_data = loc_data.get("position")
                                if pos_data:
                                    # Create Position object with percentages
                                    position = Position(
                                        percent_w=pos_data.get("percentW", 0.5),
                                        percent_h=pos_data.get("percentH", 0.5),
                                    )
                                    location.position = position
                                else:
                                    # Default to center if no position specified
                                    location.position = Position(percent_w=0.5, percent_h=0.5)

                            # Create StateLocation wrapper
                            state_location = StateLocation(
                                location=location, name=loc_data.get("name"), owner_state=state
                            )

                            # Set StateLocation flags
                            if loc_data.get("anchor"):
                                state_location.set_anchor(True)
                            if loc_data.get("fixed"):
                                state_location.set_fixed(True)

                            # Add to state
                            state.add_location(state_location)
                            self._emit_log(
                                "debug",
                                f"Added location '{loc_data.get('name')}' to state '{state.name}'",
                            )
                        except Exception as e:
                            self._emit_log("error", f"Failed to create location: {e}")

                    self.states[state_id] = state
                    self.state_manager.add_state(state)

                    # Set initial state
                    if state_data.get("isInitial"):
                        self.current_state = state_id
                        self.state_manager.set_current_state(state)
            else:
                # If Qontinui not available, just count states for testing
                for state_data in self.config.get("states", []):
                    state_id = state_data.get("id")
                    self.states[state_id] = {"name": state_data.get("name", state_id)}
                    if state_data.get("isInitial"):
                        self.current_state = state_id

            # Load execution mode configuration (REAL, MOCK, or SCREENSHOT)
            execution_settings = self.config.get("settings", {}).get("execution", {})
            exec_mode_str = execution_settings.get("executionMode", "real").upper()
            screenshot_dir = execution_settings.get("screenshotDirectory")

            # Parse execution mode from config
            if QONTINUI_AVAILABLE:
                try:
                    # Map string to MockMode enum
                    if exec_mode_str == "REAL":
                        mode = MockMode.REAL
                    elif exec_mode_str == "MOCK":
                        mode = MockMode.MOCK
                    elif exec_mode_str == "SCREENSHOT":
                        mode = MockMode.SCREENSHOT
                    else:
                        self._emit_log(
                            "warning",
                            f"Unknown execution mode '{exec_mode_str}', defaulting to REAL",
                        )
                        mode = MockMode.REAL

                    # Create ExecutionModeConfig
                    self.execution_mode = ExecutionModeConfig(
                        mode=mode, screenshot_dir=screenshot_dir
                    )

                    # Set global execution mode
                    set_execution_mode(self.execution_mode)

                    # Get controller instance
                    self.controller = get_controller()

                    self._emit_log(
                        "info", f"Execution mode set to: {self.execution_mode.mode.value}"
                    )
                    if self.execution_mode.is_screenshot_mode():
                        self._emit_log(
                            "info", f"Screenshot directory: {self.execution_mode.screenshot_dir}"
                        )

                except Exception as e:
                    self._emit_log(
                        "warning",
                        f"Failed to initialize execution mode: {e}. Defaulting to REAL mode.",
                    )
                    self.execution_mode = ExecutionModeConfig(mode=MockMode.REAL)
                    set_execution_mode(self.execution_mode)
                    self.controller = get_controller()

            # Check for graph execution setting (v2.0.0)
            self.use_graph_execution = execution_settings.get("useGraphExecution", False)

            # If graph execution is enabled and library is available, parse config with library's parser
            if self.use_graph_execution and QONTINUI_AVAILABLE:
                try:
                    config_parser = ConfigParser()
                    self.qontinui_config = config_parser.parse_config(self.config)
                    self.action_executor = ActionExecutor(
                        self.qontinui_config, use_graph_execution=True
                    )
                    self._emit_log(
                        "info", "Graph execution mode enabled - using library's ActionExecutor"
                    )
                except Exception as e:
                    self._emit_log(
                        "warning",
                        f"Failed to initialize graph executor: {e}. Falling back to manual execution.",
                    )
                    self.use_graph_execution = False
                    self.action_executor = None

            # Process workflows (v2.0.0) or processes (v1.0.0) - action sequences
            # Support both v2.0.0 (workflows) and v1.0.0 (processes) formats
            workflow_data = self.config.get("workflows", self.config.get("processes", []))
            for workflow in workflow_data:
                workflow_id = workflow.get("id")
                actions = workflow.get("actions", [])
                self.workflows[workflow_id] = actions

            # Process transitions
            for trans_data in self.config.get("transitions", []):
                trans_id = trans_data.get("id")
                trans_type = trans_data.get("type")

                if trans_type == "FromTransition":
                    from_state_id = trans_data.get("fromState")
                    to_state_id = trans_data.get("toState")

                    if from_state_id in self.states and to_state_id in self.states:
                        # Store transition data directly for simpler processing
                        self.transitions[trans_id] = {
                            "type": "FromTransition",
                            "from_state": from_state_id,
                            "to_state": to_state_id,
                            "processes": trans_data.get("processes", []),
                            "name": trans_data.get("name", trans_id),
                        }
                elif trans_type == "ToTransition":
                    to_state_id = trans_data.get("toState")
                    if to_state_id in self.states:
                        self.transitions[trans_id] = {
                            "type": "ToTransition",
                            "to_state": to_state_id,
                            "processes": trans_data.get("processes", []),
                            "name": trans_data.get("name", trans_id),
                        }

            config_info = {
                "path": config_path,
                "version": self.config.get("version", "unknown"),
                "name": self.config.get("metadata", {}).get("name", "Unnamed"),
                "states": len(self.states),
                "workflows": len(self.workflows),  # v2.0.0 terminology
                "processes": len(self.workflows),  # Backward compatibility
                "transitions": len(self.transitions),
                "images": len(self.images),
                "execution_mode": "graph" if self.use_graph_execution else "sequential",
                "graph_execution_available": QONTINUI_AVAILABLE
                and self.action_executor is not None,
                "mock_mode": self.execution_mode.mode.value if self.execution_mode else "real",
                "is_mock_mode": self.execution_mode.is_mock() if self.execution_mode else False,
                "is_screenshot_mode": (
                    self.execution_mode.is_screenshot_mode() if self.execution_mode else False
                ),
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
        """Execute a single action using Qontinui."""
        action_type = action_data.get("type")
        config = action_data.get("config", {})

        self._emit_log("info", f"Executing action: {action_type}")

        # Handle missing actions library
        if not QONTINUI_AVAILABLE or not hasattr(self, "actions"):
            self._emit_log("warning", f"Simulating action: {action_type}")
            time.sleep(0.5)  # Simulate action delay
            return True

        try:
            self._emit_event(
                EventType.ACTION_STARTED,
                {"action_id": action_data.get("id"), "action_type": action_type},
            )

            if action_type == "CLICK":
                target = config.get("target", {})
                self._emit_log("info", f"CLICK action - target type: {target.get('type')}")
                if target.get("type") == "image":
                    image_id = target.get("imageId")
                    self._emit_log("info", f"CLICK - Looking for image: {image_id}")
                    if image_id in self.images:
                        # Get similarity/threshold from target config (default 0.9)
                        threshold = target.get("threshold", config.get("similarity", 0.9))

                        # Find image on screen
                        matches = Find(self.images[image_id]).find_all()

                        # If no matches, get best match info anyway
                        best_match_info = None
                        if not matches:
                            best_match_info = self._get_best_match_regardless_of_threshold(image_id)

                        # Emit image recognition event
                        self._emit_image_recognition_event(
                            image_id, matches, threshold, best_match_info
                        )

                        if matches:
                            # Click on first match
                            location = matches[0].location
                            self.actions.click(location)
                            self._emit_log("info", f"Clicked at {location}")
                        else:
                            self._emit_log("warning", f"Image {image_id} not found on screen")
                            return False
                elif target.get("type") == "coordinates":
                    x = target.get("x", 0)
                    y = target.get("y", 0)
                    location = Location(x, y)
                    self.actions.click(location)
                    self._emit_log("info", f"Clicked at ({x}, {y})")

            elif action_type == "TYPE":
                text = config.get("text", "")
                clear_before = config.get("clear_before", False)
                press_enter = config.get("press_enter", False)
                pause_before_begin = (
                    config.get("pause_before_begin", 0) / 1000.0
                )  # Convert ms to seconds
                pause_after_end = config.get("pause_after_end", 0) / 1000.0  # Convert ms to seconds

                # Apply pause before beginning (matches Qontinui's pause_before_begin)
                if pause_before_begin > 0:
                    self._emit_log("debug", f"Pausing {pause_before_begin}s before typing")
                    time.sleep(pause_before_begin)

                # Clear existing text if requested
                if clear_before:
                    # Standard approach: Select all text (Ctrl+A) then type over it
                    # This works across most applications and operating systems
                    self._emit_log("info", "Clearing text field (Ctrl+A)")
                    if hasattr(self.actions, "key_combo"):
                        self.actions.key_combo(["ctrl", "a"])
                    elif hasattr(self.actions, "hotkey"):
                        self.actions.hotkey("ctrl", "a")
                    else:
                        # Fallback: Try to select all text manually
                        self._emit_log(
                            "warning", "Key combo not available, attempting manual select-all"
                        )
                        # This is a simplified fallback - actual implementation would depend on the library
                    time.sleep(0.1)  # Small delay to ensure selection completes

                # Process special key placeholders
                processed_text = self._process_special_keys(text)
                if hasattr(self.actions, "type_text"):
                    self.actions.type_text(processed_text)
                elif hasattr(self.actions, "type"):
                    self.actions.type(processed_text)
                self._emit_log("info", f"Typed: {text}")

                # Note: The {ENTER} placeholder in the text is already handled by _process_special_keys
                # The press_enter flag is kept for backward compatibility but may be redundant
                # if the frontend adds {ENTER} to the text when press_enter is checked
                if press_enter and not text.endswith("{ENTER}"):
                    # Only press Enter if it wasn't already in the text as a placeholder
                    self._emit_log("info", "Pressing Enter key (from press_enter flag)")
                    if hasattr(self.actions, "press"):
                        self.actions.press("enter")
                    elif hasattr(self.actions, "key_press"):
                        self.actions.key_press("enter")
                    elif hasattr(self.actions, "type_text"):
                        self.actions.type_text("\n")
                    elif hasattr(self.actions, "type"):
                        self.actions.type("\n")

                # Apply pause after end (matches Qontinui's pause_after_end)
                if pause_after_end > 0:
                    self._emit_log("debug", f"Pausing {pause_after_end}s after typing")
                    time.sleep(pause_after_end)

            elif action_type == "WAIT":
                duration = config.get("duration", 1000) / 1000.0  # Convert ms to seconds
                time.sleep(duration)
                self._emit_log("info", f"Waited {duration} seconds")

            elif action_type == "FIND":
                image_id = config.get("image") or config.get("imageId")
                self._emit_log("info", f"FIND action - Looking for image: {image_id}")
                if image_id and image_id in self.images:
                    # Get similarity/threshold from config (default 0.9)
                    threshold = config.get("similarity", 0.9)

                    # Perform find operation
                    matches = Find(self.images[image_id]).find_all()

                    # If no matches, get best match info anyway
                    best_match_info = None
                    if not matches:
                        best_match_info = self._get_best_match_regardless_of_threshold(image_id)

                    # Emit image recognition event
                    self._emit_image_recognition_event(
                        image_id, matches, threshold, best_match_info
                    )

                    if matches:
                        self._emit_log("info", f"Found {len(matches)} matches for image {image_id}")
                        self._emit_event(
                            EventType.MATCH_FOUND, {"image_id": image_id, "matches": len(matches)}
                        )
                    else:
                        self._emit_log("warning", f"Image {image_id} not found on screen")
                        return False
                else:
                    self._emit_log("warning", "Image not specified or not loaded for FIND action")
                    return False

            elif action_type == "SCROLL":
                direction = config.get("direction", "down")
                amount = config.get("amount", 3)
                # Simple scroll simulation
                self._emit_log("info", f"Scrolling {direction} by {amount} units")
                time.sleep(0.5)  # Simulate scroll time

            elif action_type == "GO_TO_STATE":
                state_id = config.get("state")
                if state_id and state_id in self.states:
                    self.current_state = state_id
                    if QONTINUI_AVAILABLE:
                        self.state_manager.set_current_state(self.states[state_id])
                    self._emit_event(
                        EventType.STATE_CHANGED,
                        {
                            "from_state": self.current_state,
                            "to_state": state_id,
                            "transition": "GO_TO_STATE action",
                        },
                    )
                    self._emit_log("info", f"Changed to state: {state_id}")
                else:
                    self._emit_log("warning", f"State {state_id} not found")
                    return False

            elif action_type == "RUN_PROCESS":
                # Support both 'process' (v1.0.0) and 'workflow' (v2.0.0) config keys
                workflow_id = config.get("workflow") or config.get("process")
                if workflow_id:
                    self._emit_log("info", f"RUN_PROCESS - Running workflow: {workflow_id}")
                    return self._execute_workflow(workflow_id)
                else:
                    self._emit_log(
                        "warning", "No workflow/process specified for RUN_PROCESS action"
                    )
                    return False

            elif action_type == "VANISH":
                image_id = config.get("image") or config.get("imageId")
                timeout = config.get("timeout", 5000) / 1000.0  # Convert to seconds
                check_interval = config.get("check_interval", 500) / 1000.0
                threshold = config.get("similarity", 0.9)

                if image_id and image_id in self.images:
                    start_time = time.time()
                    while time.time() - start_time < timeout:
                        matches = Find(self.images[image_id]).find_all()

                        # If no matches, get best match info anyway
                        best_match_info = None
                        if not matches:
                            best_match_info = self._get_best_match_regardless_of_threshold(image_id)

                        # Emit image recognition event for each check
                        self._emit_image_recognition_event(
                            image_id, matches, threshold, best_match_info
                        )

                        if not matches:
                            self._emit_log("info", f"Image {image_id} has vanished")
                            return True
                        time.sleep(check_interval)

                    self._emit_log("warning", f"Image {image_id} did not vanish within timeout")
                    return False
                else:
                    self._emit_log("warning", "Image not specified for VANISH action")

            elif action_type == "KEY":
                key = config.get("key", "")
                self.actions.key_press(key)
                self._emit_log("info", f"Pressed key: {key}")

            elif action_type == "DRAG":
                from_target = config.get("from", {})
                to_target = config.get("to", {})
                threshold = config.get("similarity", 0.9)

                # Get from location
                from_loc = None
                if from_target.get("type") == "image":
                    image_id = from_target.get("imageId")
                    if image_id in self.images:
                        matches = Find(self.images[image_id]).find_all()

                        # If no matches, get best match info anyway
                        best_match_info = None
                        if not matches:
                            best_match_info = self._get_best_match_regardless_of_threshold(image_id)

                        # Emit image recognition event for from location
                        self._emit_image_recognition_event(
                            image_id, matches, threshold, best_match_info
                        )
                        if matches:
                            from_loc = matches[0].location
                elif from_target.get("type") == "coordinates":
                    from_loc = Location(from_target.get("x", 0), from_target.get("y", 0))

                # Get to location
                to_loc = None
                if to_target.get("type") == "image":
                    image_id = to_target.get("imageId")
                    if image_id in self.images:
                        matches = Find(self.images[image_id]).find_all()

                        # If no matches, get best match info anyway
                        best_match_info = None
                        if not matches:
                            best_match_info = self._get_best_match_regardless_of_threshold(image_id)

                        # Emit image recognition event for to location
                        self._emit_image_recognition_event(
                            image_id, matches, threshold, best_match_info
                        )
                        if matches:
                            to_loc = matches[0].location
                elif to_target.get("type") == "coordinates":
                    to_loc = Location(to_target.get("x", 0), to_target.get("y", 0))

                if from_loc and to_loc:
                    self.actions.drag(from_loc, to_loc)
                    self._emit_log("info", f"Dragged from {from_loc} to {to_loc}")
                else:
                    self._emit_log("warning", "Could not find drag locations")
                    return False

            self._emit_event(
                EventType.ACTION_COMPLETED, {"action_id": action_data.get("id"), "success": True}
            )
            return True

        except Exception as e:
            self._emit_event(
                EventType.ACTION_COMPLETED,
                {"action_id": action_data.get("id"), "success": False, "error": str(e)},
            )
            self._emit_log("error", f"Action failed: {e}")
            return False

    def _execute_workflow(self, workflow_id: str) -> bool:
        """Execute a workflow using the library's ActionExecutor or manual fallback."""

        if self.use_graph_execution and self.action_executor:
            try:
                # Use library's graph executor
                self._emit_log("info", f"Executing workflow '{workflow_id}' using graph execution")
                result = self.action_executor.execute_workflow(workflow_id)

                # Emit events based on result
                if result.get("success"):
                    self._emit_event(
                        EventType.WORKFLOW_COMPLETED,
                        {
                            "workflow_id": workflow_id,
                            "success": True,
                            "actions_executed": result.get("actions_executed", 0),
                        },
                    )
                    self._emit_event(
                        EventType.PROCESS_COMPLETED, {"process_id": workflow_id, "success": True}
                    )
                    return True
                else:
                    self._emit_event(
                        EventType.WORKFLOW_COMPLETED,
                        {
                            "workflow_id": workflow_id,
                            "success": False,
                            "error": result.get("error"),
                        },
                    )
                    self._emit_event(
                        EventType.PROCESS_COMPLETED, {"process_id": workflow_id, "success": False}
                    )
                    return False

            except Exception as e:
                self._emit_log("error", f"Graph execution failed: {str(e)}")
                self._emit_log("info", "Falling back to manual execution")
                # Fall back to manual execution
                return self._execute_workflow_manual(workflow_id)
        else:
            # No library available or graph execution disabled - use manual execution
            return self._execute_workflow_manual(workflow_id)

    def _execute_workflow_manual(self, workflow_id: str) -> bool:
        """Manual workflow execution (legacy/fallback). v2.0.0 terminology."""
        if workflow_id not in self.workflows:
            self._emit_log("error", f"Workflow {workflow_id} not found")
            return False

        # Emit both new and old event types for backward compatibility
        self._emit_event(
            EventType.WORKFLOW_STARTED, {"workflow_id": workflow_id, "workflow_name": workflow_id}
        )
        self._emit_event(
            EventType.PROCESS_STARTED, {"process_id": workflow_id, "process_name": workflow_id}
        )

        actions = self.workflows[workflow_id]
        success = True

        for action in actions:
            if not self.is_running:
                break

            if not self._execute_action(action):
                success = False
                break

            # Small delay between actions
            time.sleep(0.5)

        # Emit both new and old event types for backward compatibility
        self._emit_event(
            EventType.WORKFLOW_COMPLETED, {"workflow_id": workflow_id, "success": success}
        )
        self._emit_event(
            EventType.PROCESS_COMPLETED, {"process_id": workflow_id, "success": success}
        )

        return success

    def _execute_process(self, process_id: str) -> bool:
        """Deprecated: Use _execute_workflow instead. Kept for backward compatibility."""
        return self._execute_workflow(process_id)

    def _run_state_machine(self):
        """Run the state machine execution."""
        try:
            # Execute transitions from current state
            executed_count = 0
            max_iterations = 100  # Prevent infinite loops

            while self.is_running and executed_count < max_iterations:
                transition_found = False

                # Find transitions from current state
                for transition in self.transitions.values():
                    if (
                        transition.get("type") == "FromTransition"
                        and transition.get("from_state") == self.current_state
                    ):
                        self._emit_log("info", f"Executing transition: {transition.get('name')}")

                        # Execute workflows in transition (processes field kept for backward compatibility)
                        for workflow_id in transition.get("processes", []):
                            if not self._execute_workflow(workflow_id):
                                self._emit_log("error", f"Workflow {workflow_id} failed")
                                self.is_running = False
                                break
                        else:
                            # Only continue if all workflows succeeded
                            pass

                        if self.is_running:
                            # Change state
                            old_state = self.current_state
                            self.current_state = transition.get("to_state")

                            # Update state manager if available
                            if self.current_state in self.states and QONTINUI_AVAILABLE:
                                self.state_manager.set_current_state(
                                    self.states[self.current_state]
                                )

                            self._emit_event(
                                EventType.STATE_CHANGED,
                                {
                                    "from_state": old_state,
                                    "to_state": self.current_state,
                                    "transition": transition.get("name"),
                                },
                            )

                            # Execute ToTransitions for the new state
                            for to_transition in self.transitions.values():
                                if (
                                    to_transition.get("type") == "ToTransition"
                                    and to_transition.get("to_state") == self.current_state
                                ):
                                    self._emit_log(
                                        "info",
                                        f"Executing ToTransition: {to_transition.get('name')}",
                                    )
                                    for workflow_id in to_transition.get("processes", []):
                                        if not self._execute_workflow(workflow_id):
                                            self._emit_log(
                                                "error",
                                                f"Workflow {workflow_id} in ToTransition failed",
                                            )

                            # Check if final state
                            for state_data in self.config.get("states", []):
                                if state_data.get("id") == self.current_state and state_data.get(
                                    "isFinal"
                                ):
                                    self._emit_log("info", "Reached final state")
                                    self.is_running = False
                                    break

                            transition_found = True
                            executed_count += 1
                            break

                if not transition_found:
                    self._emit_log("info", "No more transitions available from current state")
                    break

            self._emit_event(
                EventType.EXECUTION_COMPLETED,
                {
                    "success": True,
                    "final_state": self.current_state,
                    "transitions_executed": executed_count,
                },
            )

        except Exception as e:
            self._emit_event(
                EventType.ERROR,
                {
                    "message": "State machine execution failed",
                    "details": str(e),
                    "traceback": traceback.format_exc(),
                },
            )
        finally:
            self.is_running = False

    def _run_workflow(self, workflow_id: str):
        """Run a specific workflow directly. v2.0.0 terminology."""
        try:
            self._emit_log("info", f"Starting workflow execution: {workflow_id}")

            success = self._execute_workflow(workflow_id)

            self._emit_event(
                EventType.EXECUTION_COMPLETED,
                {
                    "success": success,
                    "workflow_id": workflow_id,
                    "process_id": workflow_id,
                    "mode": "workflow",
                },
            )

        except Exception as e:
            self._emit_event(
                EventType.ERROR,
                {
                    "message": "Workflow execution failed",
                    "details": str(e),
                    "traceback": traceback.format_exc(),
                },
            )
        finally:
            self.is_running = False

    def _run_process(self, process_id: str):
        """Deprecated: Use _run_workflow instead. Kept for backward compatibility."""
        self._run_workflow(process_id)

    def start_execution(
        self, mode: str = "state_machine", process_id: str = None, workflow_id: str = None
    ) -> bool:
        """Start automation execution.

        Args:
            mode: Execution mode - 'state_machine', 'workflow', or 'process' (deprecated)
            process_id: Process ID for backward compatibility (deprecated, use workflow_id)
            workflow_id: Workflow ID for v2.0.0 format
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

        try:
            self.is_running = True

            self._emit_event(
                EventType.EXECUTION_STARTED, {"mode": mode, "initial_state": self.current_state}
            )

            if mode == "state_machine":
                # Run state machine in separate thread
                execution_thread = threading.Thread(target=self._run_state_machine)
                execution_thread.daemon = True
                execution_thread.start()
            elif mode in ("workflow", "process"):
                # Support both 'workflow' (v2.0.0) and 'process' (v1.0.0) modes
                # workflow_id takes precedence over process_id
                target_id = workflow_id or process_id
                if not target_id:
                    self._emit_log("error", "Workflow ID required for workflow/process mode")
                    return False
                execution_thread = threading.Thread(target=self._run_workflow, args=(target_id,))
                execution_thread.daemon = True
                execution_thread.start()
            else:
                self._emit_log("error", f"Unsupported execution mode: {mode}")
                return False

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

        if cmd_type == "load":
            config_path = params.get("config_path")
            success = self.load_configuration(config_path)
            return {"success": success}

        elif cmd_type == "start":
            mode = params.get("mode", "state_machine")
            # Support both workflow_id (v2.0.0) and process_id (v1.0.0)
            workflow_id = params.get("workflow_id")
            process_id = params.get("process_id")
            success = self.start_execution(mode, process_id=process_id, workflow_id=workflow_id)
            return {"success": success}

        elif cmd_type == "stop":
            self.stop_execution()
            return {"success": True}

        elif cmd_type == "status":
            return {
                "is_running": self.is_running,
                "current_state": self.current_state,
                "config_loaded": self.config is not None,
                "library_available": QONTINUI_AVAILABLE,
            }

        elif cmd_type == "start_recording":
            return self._handle_start_recording(params)

        elif cmd_type == "stop_recording":
            return self._handle_stop_recording()

        elif cmd_type == "recording_status":
            return self._handle_recording_status()

        else:
            return {"success": False, "error": f"Unknown command: {cmd_type}"}

    def _handle_start_recording(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle start_recording command.

        Args:
            params: Command parameters containing 'base_dir'

        Returns:
            Response with success status and snapshot directory
        """
        if not QONTINUI_AVAILABLE or not self.controller:
            return {"success": False, "error": "Qontinui library or controller not available"}

        try:
            base_dir = params.get("base_dir", "/tmp/qontinui-snapshots")

            # Start recording
            snapshot_dir = self.controller.start_recording(base_dir)

            # Emit event
            self._emit_event(
                EventType.RECORDING_STARTED,
                {"snapshot_directory": snapshot_dir, "base_directory": base_dir},
            )

            self._emit_log("info", f"Recording started: {snapshot_dir}")

            return {"success": True, "snapshot_directory": snapshot_dir}

        except Exception as e:
            error_msg = f"Failed to start recording: {str(e)}"
            self._emit_log("error", error_msg)
            return {"success": False, "error": error_msg}

    def _handle_stop_recording(self) -> dict[str, Any]:
        """Handle stop_recording command.

        Returns:
            Response with success status and snapshot directory
        """
        if not QONTINUI_AVAILABLE or not self.controller:
            return {"success": False, "error": "Qontinui library or controller not available"}

        try:
            # Stop recording
            snapshot_dir = self.controller.stop_recording()

            if snapshot_dir:
                # Get final statistics
                stats = {"snapshot_directory": snapshot_dir}

                # Emit event
                self._emit_event(EventType.RECORDING_STOPPED, stats)

                self._emit_log("info", f"Recording stopped: {snapshot_dir}")

                return {"success": True, "snapshot_directory": snapshot_dir}
            else:
                return {"success": False, "error": "No recording in progress"}

        except Exception as e:
            error_msg = f"Failed to stop recording: {str(e)}"
            self._emit_log("error", error_msg)
            return {"success": False, "error": error_msg}

    def _handle_recording_status(self) -> dict[str, Any]:
        """Handle recording_status command.

        Returns:
            Response with recording status and statistics
        """
        if not QONTINUI_AVAILABLE or not self.controller:
            return {"success": False, "error": "Qontinui library or controller not available"}

        try:
            is_recording = self.controller.is_recording()
            stats = self.controller.get_recording_stats() if is_recording else None

            return {"success": True, "is_recording": is_recording, "statistics": stats}

        except Exception as e:
            error_msg = f"Failed to get recording status: {str(e)}"
            self._emit_log("error", error_msg)
            return {"success": False, "error": error_msg}

    def __del__(self):
        """Clean up temp directory on exit."""
        if self.temp_dir and os.path.exists(self.temp_dir):
            import contextlib
            import shutil

            with contextlib.suppress(Exception):
                shutil.rmtree(self.temp_dir)


def main():
    """Main entry point for the Qontinui executor."""
    executor = QontinuiExecutor()

    # Read commands from stdin
    for line in sys.stdin:
        try:
            command = json.loads(line.strip())

            if command.get("type") == "command":
                response = executor.handle_command(command)
                response["id"] = command.get("id")
                response["type"] = "response"
                sys.stdout.write(json.dumps(response) + "\n")
                sys.stdout.flush()

        except json.JSONDecodeError as e:
            executor._emit_event(
                EventType.ERROR, {"message": "Invalid JSON command", "details": str(e)}
            )
        except Exception:
            executor._emit_event(
                EventType.ERROR,
                {
                    "message": "Command execution failed",
                    "details": traceback.format_exc(),
                },
            )


if __name__ == "__main__":
    main()
