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

# Add qontinui library src directory to path
# This file is in: qontinui_parent/qontinui-runner/python-bridge/qontinui_executor.py
# We need to add: qontinui_parent/qontinui/src
qontinui_src_path = Path(__file__).parent.parent.parent / "qontinui" / "src"
sys.path.insert(0, str(qontinui_src_path))

# Debug: Print the resolved path
print(json.dumps({
    "type": "event",
    "event": "log",
    "timestamp": time.time(),
    "sequence": 0,
    "data": {
        "level": "debug",
        "message": f"Qontinui source path added to sys.path: {qontinui_src_path} (exists: {qontinui_src_path.exists()})"
    }
}), flush=True)

try:
    from qontinui import Find, FluentActions, Image, Location
    from qontinui.config import get_settings, enable_mock_mode, disable_mock_mode
    from qontinui import navigation_api, registry
    # json_executor and wrappers.get_controller don't exist - commented out
    # from qontinui.json_executor.action_executor import ActionExecutor
    # from qontinui.json_executor.config_parser import ConfigParser
    # from qontinui.wrappers import get_controller

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
    RECORDING_STARTED = "recording_started"
    RECORDING_STOPPED = "recording_stopped"


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

        if QONTINUI_AVAILABLE:
            self.actions = FluentActions()
            self.settings = get_settings()

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

        # Try to get template size from Image object
        try:
            if hasattr(image_obj, "width") and hasattr(image_obj, "height"):
                template_size = f"{image_obj.width}x{image_obj.height}"
            else:
                template_size = "unknown"
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
                "image_path": image_id,
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
                f"Emitting IMAGE_RECOGNITION event (FOUND): {image_id}, confidence: {confidence}",
            )
            self._emit_event(EventType.IMAGE_RECOGNITION, event_data)
        else:
            # Build event data for no match found
            event_data = {
                "image_path": image_id,
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
                f"Emitting IMAGE_RECOGNITION event (NOT FOUND): {image_id}, best_match: {best_match_info is not None}",
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
                        self.images[img_id] = image_obj
                        # Register image in library's registry for state/transition loading
                        registry.register_image(img_id, image_obj)
                        self._emit_log("debug", f"Loaded and registered image: {img_id} -> {img_path}")
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
                            underlying_image_id = first_pattern.get("image")

                            self._emit_log("debug", f"State image {state_image_id} -> underlying image {underlying_image_id}")

                            if underlying_image_id and underlying_image_id in self.images:
                                # Map state image ID to the underlying image object
                                self.images[state_image_id] = self.images[underlying_image_id]
                                self._emit_log("debug", f"Mapped state image {state_image_id} -> {underlying_image_id}")
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
                self.workflows[workflow_id] = actions

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
                except Exception as e:
                    self._emit_log("warning", f"Failed to initialize navigation: {e}")
                    import traceback
                    self._emit_log("debug", f"Traceback: {traceback.format_exc()}")

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

                # Handle string targets (like "Last Find Result")
                if isinstance(target, str):
                    if target == "Last Find Result":
                        if self._last_find_location:
                            self.actions.click(self._last_find_location)
                            self._emit_log("info", f"Clicked at last find location: {self._last_find_location}")
                        else:
                            self._emit_log("error", "CLICK - No previous find result available")
                            return False
                    else:
                        self._emit_log("error", f"CLICK - Unknown string target: {target}")
                        return False
                # Handle dictionary targets (image or coordinates)
                elif isinstance(target, dict):
                    self._emit_log("info", f"CLICK action - target type: {target.get('type')}")
                    if target.get("type") == "image":
                        image_id = target.get("imageId")
                        self._emit_log("info", f"CLICK - Looking for image: {image_id}")
                        if image_id in self.images:
                            # Get similarity/threshold from target config (default 0.9)
                            threshold = target.get("threshold", config.get("similarity", 0.9))

                            # Find image on screen with configured threshold
                            results = Find(self.images[image_id]).similarity(threshold).execute()
                            matches = results.matches

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
                # Fallback: check for x,y directly in config (legacy format)
                elif "x" in config and "y" in config:
                    x = config.get("x", 0)
                    y = config.get("y", 0)
                    location = Location(x, y)
                    self.actions.click(location)
                    self._emit_log("info", f"Clicked at ({x}, {y}) [legacy format]")

            elif action_type == "TYPE":
                # Get text from config or from stateString
                text_source = config.get("textSource", "direct")
                if text_source == "stateString":
                    # Load text from state string
                    selected_state = config.get("selectedState")
                    selected_strings = config.get("selectedStateStrings", [])
                    text = ""

                    if selected_state and selected_strings and self.config:
                        # Find the state in config
                        states = self.config.get("states", [])
                        for state in states:
                            if state.get("name") == selected_state or state.get("id") == selected_state:
                                # Find the string in the state
                                for string_id in selected_strings:
                                    for state_string in state.get("strings", []):
                                        if state_string.get("id") == string_id:
                                            text = state_string.get("value", "")
                                            self._emit_log("debug", f"Loaded text from stateString {string_id}: '{text}'")
                                            break
                                    if text:
                                        break
                                break

                    if not text:
                        self._emit_log("warning", f"Could not load text from stateString (state={selected_state}, strings={selected_strings})")
                else:
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
                    # FluentActions uses builder pattern - must call execute() to actually type
                    self.actions.type_text(processed_text).execute()
                    self.actions.clear()  # Clear the chain for next use
                elif hasattr(self.actions, "type"):
                    self.actions.type(processed_text)
                self._emit_log("info", f"Typed: {text}")

                # Note: The {ENTER} placeholder in the text is already handled by _process_special_keys
                # The press_enter flag is kept for backward compatibility but may be redundant
                # if the frontend adds {ENTER} to the text when press_enter is checked
                if press_enter and not text.endswith("{ENTER}"):
                    # Only press Enter if it wasn't already in the text as a placeholder
                    self._emit_log("info", "Pressing Enter key (from press_enter flag)")
                    if hasattr(self.actions, "key"):
                        # FluentActions uses builder pattern - must call execute()
                        self.actions.key("enter").execute()
                        self.actions.clear()
                    elif hasattr(self.actions, "key_press"):
                        self.actions.key_press("enter")
                    elif hasattr(self.actions, "type_text"):
                        self.actions.type_text("\n").execute()
                        self.actions.clear()
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
                    if "threshold" in config:
                        threshold = config["threshold"]

                    self._emit_log("debug", f"FIND - Using threshold: {threshold}, config keys: {config.keys()}")
                    self._emit_log("debug", f"FIND - Config similarity: {config.get('similarity')}, threshold: {config.get('threshold')}")

                    # Perform find operation with configured threshold
                    find_obj = Find(self.images[image_id]).similarity(threshold)
                    self._emit_log("debug", f"FIND - Created Find object with threshold {threshold}")

                    # Debug: Check actual threshold values AFTER setting
                    if hasattr(find_obj, '_options') and hasattr(find_obj._options, '_min_similarity'):
                        actual_min_sim = find_obj._options._min_similarity
                        self._emit_log("debug", f"FIND - FindOptions._min_similarity is actually: {actual_min_sim}")

                    # Check the pattern's threshold
                    if hasattr(find_obj, '_target') and find_obj._target:
                        pattern_threshold = getattr(find_obj._target, 'similarity_threshold', 'N/A')
                        self._emit_log("debug", f"FIND - Pattern similarity_threshold: {pattern_threshold}")

                    # Check the options
                    if hasattr(find_obj, '_options'):
                        self._emit_log("debug", f"FIND - Options object: {find_obj._options}")

                    results = find_obj.execute()
                    self._emit_log("debug", f"FIND - Execute returned results object: {results}")
                    self._emit_log("debug", f"FIND - Results type: {type(results)}, has matches: {hasattr(results, 'matches')}")

                    matches = results.matches
                    self._emit_log("debug", f"FIND - Matches: {matches}, type: {type(matches)}, len: {len(matches) if matches else 0}")

                    # If no matches, get best match info anyway
                    best_match_info = None
                    if not matches:
                        best_match_info = self._get_best_match_regardless_of_threshold(image_id)

                        # Save debug screenshots when FIND fails
                        try:
                            import os
                            from datetime import datetime
                            debug_dir = os.path.join(os.path.dirname(self.images[image_id]), "debug_screenshots")
                            os.makedirs(debug_dir, exist_ok=True)

                            # Save current screenshot
                            timestamp = datetime.now().strftime("%Y%m%d_%H%M%S")
                            screenshot_path = os.path.join(debug_dir, f"{image_id}_{timestamp}_screenshot.png")

                            # Capture and save current screen
                            from qontinui.hal.factory import HALFactory
                            screen_capture = HALFactory.get_screen_capture()
                            screenshot = screen_capture.capture_screen()
                            screenshot.save(screenshot_path)

                            # Copy template for comparison
                            import shutil
                            template_path = os.path.join(debug_dir, f"{image_id}_{timestamp}_template.png")
                            shutil.copy(self.images[image_id], template_path)

                            self._emit_log("info", f"DEBUG: Saved screenshot to {screenshot_path}")
                            self._emit_log("info", f"DEBUG: Saved template to {template_path}")
                        except Exception as e:
                            self._emit_log("warning", f"Failed to save debug screenshots: {e}")

                    # Emit image recognition event
                    self._emit_image_recognition_event(
                        image_id, matches, threshold, best_match_info
                    )

                    if matches:
                        self._emit_log("info", f"Found {len(matches)} matches for image {image_id}")
                        # Store first match location for "Last Find Result" clicks
                        self._last_find_location = matches[0].location
                        self._emit_log("debug", f"Stored last find location: {self._last_find_location}")
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
                # GO_TO_STATE action - navigate to specified states using library's navigation API
                # The library accepts state names (strings) and converts them to IDs internally
                # Check for both stateIds (preferred) and stateNames (for backward compatibility)
                state_names = config.get("stateIds", config.get("stateNames", []))

                if not state_names:
                    self._emit_log("error", "GO_TO_STATE action missing 'stateIds' or 'stateNames' in config")
                    return False

                self._emit_log("info", f"GO_TO_STATE - Navigating to states: {state_names}")

                # CRITICAL: Inject workflow executor so navigation system can execute transition workflows
                # The executor (self) can execute workflows via execute_workflow()
                navigation_api.set_workflow_executor(self)

                # Use the library's navigation API - pass state names as strings
                # The library will convert them to IDs and handle all state management internally
                success = navigation_api.open_states(state_names)

                if success:
                    self._emit_log("info", f"Successfully navigated to states: {state_names}")
                else:
                    self._emit_log("warning", f"Failed to navigate to states: {state_names}")

                return success

            elif action_type in ("RUN_WORKFLOW", "RUN_PROCESS"):
                workflow_id = config.get("workflow") or config.get("workflowId")
                if not workflow_id:
                    self._emit_log("error", f"No workflow ID in config: {config}")
                    return False

                self._emit_log("info", f"RUN_WORKFLOW - Running nested workflow: {workflow_id}")
                self._emit_log("debug", f"Nested workflow exists: {workflow_id in self.workflows}")
                if workflow_id in self.workflows:
                    self._emit_log("debug", f"Nested workflow has {len(self.workflows[workflow_id])} actions")
                return self._execute_workflow(workflow_id)

            elif action_type == "VANISH":
                image_id = config.get("image") or config.get("imageId")
                timeout = config.get("timeout", 5000) / 1000.0  # Convert to seconds
                check_interval = config.get("check_interval", 500) / 1000.0
                threshold = config.get("similarity", 0.9)

                if image_id and image_id in self.images:
                    start_time = time.time()
                    while time.time() - start_time < timeout:
                        results = Find(self.images[image_id]).similarity(threshold).execute()
                        matches = results.matches

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
                        threshold = from_target.get("threshold", config.get("similarity", 0.9))
                        results = Find(self.images[image_id]).similarity(threshold).execute()
                        matches = results.matches

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
                        threshold = to_target.get("threshold", config.get("similarity", 0.9))
                        results = Find(self.images[image_id]).similarity(threshold).execute()
                        matches = results.matches

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

            elif action_type == "IF":
                condition = config.get("condition", {})
                condition_type = condition.get("type")

                if condition_type == "image_exists":
                    image_id = condition.get("imageId")
                    threshold = condition.get("threshold", config.get("similarity", 0.9))

                    if image_id and image_id in self.images:
                        self._emit_log("debug", f"IF - Checking if image exists: {image_id}")

                        # Find image on screen
                        results = Find(self.images[image_id]).similarity(threshold).execute()
                        matches = results.matches

                        # Store location for "Last Find Result" usage
                        if matches:
                            self._last_find_location = matches[0].location
                            self._emit_log("debug", f"IF - Image found, stored location: {self._last_find_location}")
                        else:
                            self._emit_log("debug", f"IF - Image not found: {image_id}")

                        # Execute then/else actions (currently empty in the config, so we skip this)
                        # In the future, we could execute nested actions here

                    else:
                        self._emit_log("warning", f"IF - Image {image_id} not loaded")
                else:
                    self._emit_log("warning", f"IF - Unknown condition type: {condition_type}")

            elif action_type == "MOUSE_MOVE":
                target = config.get("target", {})

                # Handle string targets (like "Last Find Result")
                if isinstance(target, str):
                    if target == "Last Find Result":
                        if self._last_find_location:
                            self.actions.move(self._last_find_location)
                            self._emit_log("info", f"Moved mouse to last find location: {self._last_find_location}")
                        else:
                            self._emit_log("error", "MOUSE_MOVE - No previous find result available")
                            return False
                    else:
                        self._emit_log("error", f"MOUSE_MOVE - Unknown string target: {target}")
                        return False
                # Handle dictionary targets (image or coordinates)
                elif isinstance(target, dict):
                    if target.get("type") == "image":
                        image_id = target.get("imageId")
                        if image_id in self.images:
                            threshold = target.get("threshold", config.get("similarity", 0.9))
                            results = Find(self.images[image_id]).similarity(threshold).execute()
                            matches = results.matches

                            if matches:
                                location = matches[0].location
                                self.actions.move(location)
                                self._emit_log("info", f"Moved mouse to {location}")
                            else:
                                self._emit_log("warning", f"Image {image_id} not found for MOUSE_MOVE")
                                return False
                    elif target.get("type") == "coordinates":
                        x = target.get("x", 0)
                        y = target.get("y", 0)
                        location = Location(x, y)
                        self.actions.move(location)
                        self._emit_log("info", f"Moved mouse to ({x}, {y})")

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
        """Execute a workflow using manual execution (graph execution not available)."""
        # Note: Graph execution not available - json_executor modules don't exist
        return self._execute_workflow_manual(workflow_id)

    def _execute_workflow_manual(self, workflow_id: str) -> bool:
        """Manual workflow execution."""
        # Try local workflows first
        if workflow_id in self.workflows:
            actions = self.workflows[workflow_id]
        # Fallback to registry for inline workflows registered by transition loader
        elif QONTINUI_AVAILABLE:
            actions = registry.get_workflow(workflow_id)
            if actions is None:
                self._emit_log("error", f"Workflow {workflow_id} not found in local workflows or registry")
                return False
            # Cache it locally for future use
            self.workflows[workflow_id] = actions
            self._emit_log("debug", f"Loaded workflow {workflow_id} from registry")
        else:
            self._emit_log("error", f"Workflow {workflow_id} not found")
            return False

        self._emit_event(
            EventType.WORKFLOW_STARTED, {"workflow_id": workflow_id, "workflow_name": workflow_id}
        )

        success = True

        for action in actions:
            if not self.is_running:
                break

            if not self._execute_action(action):
                success = False
                break

            # Small delay between actions
            time.sleep(0.5)

        self._emit_event(
            EventType.WORKFLOW_COMPLETED, {"workflow_id": workflow_id, "success": success}
        )

        return success

    def execute_workflow(self, workflow_id: str) -> dict:
        """Execute a workflow and return result in navigation API expected format.

        This method is called by the navigation system when executing transitions.
        It wraps _execute_workflow() to provide the expected return format.

        Args:
            workflow_id: ID of workflow to execute

        Returns:
            dict with 'success' key: {'success': True/False}
        """
        try:
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

            success = self._execute_workflow(workflow_id)

            self._emit_event(
                EventType.EXECUTION_COMPLETED,
                {
                    "success": success,
                    "workflow_id": workflow_id,
                },
            )

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
                "is_running": self.is_running,
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
        # Note: Recording not available - controller (from wrappers) doesn't exist in qontinui
        return {"success": False, "error": "Recording not available (controller module doesn't exist)"}

    def _handle_stop_recording(self) -> dict[str, Any]:
        """Handle stop_recording command.

        Returns:
            Response with success status and snapshot directory
        """
        # Note: Recording not available - controller (from wrappers) doesn't exist in qontinui
        return {"success": False, "error": "Recording not available (controller module doesn't exist)"}

    def _handle_recording_status(self) -> dict[str, Any]:
        """Handle recording_status command.

        Returns:
            Response with recording status and statistics
        """
        # Note: Recording not available - controller (from wrappers) doesn't exist in qontinui
        return {"success": False, "error": "Recording not available (controller module doesn't exist)"}

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
