"""
Event Manager Module

Single Responsibility: Handle all event emission and translation
- Emit events to Tauri frontend via stdout
- Translate qontinui library events to frontend format
- Thread-safe event emission
- Event type enum and mappings
"""

import json
import sys
import threading
import time
from enum import Enum
from typing import Any, Dict, Optional


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
    # Web extraction events
    EXTRACTION_STARTED = "extraction_started"
    EXTRACTION_PROGRESS = "extraction_progress"
    EXTRACTION_STATE_DETECTED = "extraction_state_detected"
    EXTRACTION_ELEMENT_DETECTED = "extraction_element_detected"
    EXTRACTION_COMPLETE = "extraction_complete"
    EXTRACTION_ERROR = "extraction_error"


class EventManager:
    """
    Manages all event emission and translation.

    Responsibilities:
    - Emit events to stdout in JSON format
    - Provide thread-safe event emission
    - Translate event types from strings to enums
    - Manage event sequence numbers
    - Emit specialized event types (tree events, image recognition)
    """

    def __init__(self):
        """Initialize event manager."""
        self._sequence = 0
        self._output_lock = threading.Lock()

    def emit_event(self, event_type: EventType, data: Dict[str, Any]) -> None:
        """
        Emit event to Tauri through stdout (thread-safe).

        Args:
            event_type: Type of event to emit
            data: Event data dictionary
        """
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

    def emit_log(self, level: str, message: str) -> None:
        """
        Emit log message.

        Args:
            level: Log level (info, warning, error, debug)
            message: Log message
        """
        self.emit_event(EventType.LOG, {"level": level, "message": message})

    def emit_tree_event(self, event_type: str, node: Any, extra_data: Optional[Dict] = None) -> None:
        """
        Emit a tree-based event with full tree context.

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

    def emit_event_wrapper(self, event_type: str, data: Dict[str, Any]) -> None:
        """
        Wrapper for EventTranslator to convert string event names to EventType enum.

        This method acts as a bridge between EventTranslator (which uses string event names)
        and emit_event (which expects EventType enum values).

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

        # Emit the event using the existing method
        self.emit_event(enum_event_type, data)

    def emit_image_recognition_event(
        self, image_id: str, matches: list, threshold: float = 0.9,
        best_match_info: Optional[Dict] = None, image_obj: Any = None,
        get_state_for_image_fn: Any = None
    ) -> None:
        """
        Emit image recognition event with detailed information.

        Args:
            image_id: ID of the image being searched for
            matches: List of matches found (empty list if not found)
            threshold: Similarity threshold used for matching
            best_match_info: Optional dict with best match info even if it didn't meet threshold
            image_obj: Image object for extracting template size
            get_state_for_image_fn: Function to get state name for an image
        """
        # Get display name for the image (prefer name over ID)
        display_name = image_id  # Default to ID
        if image_obj:
            if hasattr(image_obj, "name") and image_obj.name:
                display_name = image_obj.name
            elif isinstance(image_obj, dict) and "name" in image_obj:
                display_name = image_obj["name"]

        # Try to get template size from Image object
        template_size = ""
        if image_obj:
            try:
                if hasattr(image_obj, "mat") and image_obj.mat is not None:
                    # OpenCV mat format: (height, width, channels)
                    template_size = f"{image_obj.mat.shape[1]}, {image_obj.mat.shape[0]}"
                elif hasattr(image_obj, '_pattern') and hasattr(image_obj._pattern, 'mat'):
                    template_size = f"{image_obj._pattern.mat.shape[1]}, {image_obj._pattern.mat.shape[0]}"
                elif hasattr(image_obj, "width") and hasattr(image_obj, "height"):
                    template_size = f"{image_obj.width}, {image_obj.height}"
            except Exception:
                pass

        # Try to get screenshot size
        screenshot_size = "1920, 1080"  # Default from screen capture
        try:
            from PIL import ImageGrab
            screenshot = ImageGrab.grab()
            screenshot_size = f"{screenshot.width}, {screenshot.height}"
        except Exception:
            pass

        # Get state information for this image
        state_name = None
        if get_state_for_image_fn:
            state_name = get_state_for_image_fn(image_id)

        if matches:
            # Get confidence from first match
            first_match = matches[0]
            confidence = getattr(first_match, "score", threshold)
            location = f"({getattr(first_match, 'x', 0)}, {getattr(first_match, 'y', 0)})"

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

            self.emit_event(EventType.IMAGE_RECOGNITION, event_data)
        else:
            # Build event data for no match found
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

                event_data["confidence"] = best_confidence  # Send raw 0.0-1.0 value
                event_data["best_match_location"] = f"({best_x}, {best_y})"
                event_data["gap"] = threshold - best_confidence
                event_data["percent_off"] = (
                    ((threshold - best_confidence) / threshold) if threshold > 0 else 0
                )

            # Emit event
            self.emit_event(EventType.IMAGE_RECOGNITION, event_data)
