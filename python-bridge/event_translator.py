#!/usr/bin/env python3
"""
Event translator for converting qontinui library events to frontend JSON format.

This module provides the EventTranslator class that registers callbacks with the
qontinui library's event system and translates them into the format expected by
the Tauri frontend.
"""

import sys
from io import StringIO
from typing import Any, Callable

try:
    from qontinui.reporting import EventType as QontinuiEventType, register_callback
    QONTINUI_AVAILABLE = True
except ImportError:
    QONTINUI_AVAILABLE = False
    QontinuiEventType = None


class EventTranslator:
    """Translates qontinui library events to frontend JSON format.

    This class acts as a bridge between the qontinui library's internal event
    system and the frontend's expected event format. It registers callbacks
    for library events and translates them into frontend-compatible JSON events.

    Key features:
    - Dependency injection for testability (accepts emitter callback)
    - Proper stdout restoration to handle qontinui's output capture
    - Extensible design for adding new event types
    - No global state (all state is instance-based)

    Example:
        >>> def emit_fn(event_type: str, data: dict):
        ...     print(f"Event: {event_type}, Data: {data}")
        ...
        >>> translator = EventTranslator(emit_fn)
        >>> translator.register_all_callbacks()
        >>> # Now library events will be translated and emitted via emit_fn
    """

    def __init__(self, emitter: Callable[[str, dict[str, Any]], None],
                 state_lookup: Callable[[str], str | None] | None = None,
                 hierarchy_lookup: Callable[[], dict[str, Any]] | None = None,
                 image_data_lookup: Callable[[str], str | None] | None = None):
        """Initialize the event translator.

        Args:
            emitter: Callback function that accepts (event_type: str, data: dict)
                    and emits the event to the frontend. This is typically the
                    QontinuiExecutor._emit_event method bound to EventType enum.
            state_lookup: Optional callback to look up state name for an image ID.
                         Accepts image_id and returns state name or None.
            hierarchy_lookup: Optional callback to get current execution hierarchy.
                             Returns dict with parent_id, nesting_level, workflow_name, is_expandable.
            image_data_lookup: Optional callback to look up base64 image data for an image ID.
                              Accepts image_id and returns base64 data string or None.

        Note:
            The emitter should handle converting the event_type string to the
            appropriate EventType enum value if needed.
        """
        self.emitter = emitter
        self.state_lookup = state_lookup
        self.hierarchy_lookup = hierarchy_lookup
        self.image_data_lookup = image_data_lookup
        self._real_stdout = sys.stdout

        if not QONTINUI_AVAILABLE:
            raise RuntimeError(
                "EventTranslator requires qontinui library to be installed. "
                "Cannot register callbacks without qontinui.reporting module."
            )

    def register_all_callbacks(self) -> None:
        """Register all event translation callbacks with the qontinui library.

        This method registers callbacks for all supported event types. Call this
        once during initialization to set up event translation.

        Example:
            >>> translator = EventTranslator(my_emit_function)
            >>> translator.register_all_callbacks()
        """
        # Register MATCH_ATTEMPTED callback for image recognition events
        register_callback(QontinuiEventType.MATCH_ATTEMPTED, self.on_match_attempted)

        # Register TEXT_TYPED callback for typing action events
        register_callback(QontinuiEventType.TEXT_TYPED, self.on_text_typed)

        # Register ACTION_COMPLETED callback to add hierarchy to library action events
        register_callback(QontinuiEventType.ACTION_COMPLETED, self.on_action_completed_library)

        # Future: Additional callbacks can be registered here
        # register_callback(QontinuiEventType.ACTION_STARTED, self.on_action_started)
        # register_callback(QontinuiEventType.MOUSE_CLICKED, self.on_mouse_clicked)

    def on_match_attempted(self, event) -> None:
        """Translate MATCH_ATTEMPTED event to IMAGE_RECOGNITION format.

        Converts qontinui library's MATCH_ATTEMPTED event into the frontend's
        IMAGE_RECOGNITION event format with properly formatted data fields.

        Args:
            event: qontinui Event object with type=MATCH_ATTEMPTED
                  Expected data fields:
                  - image_id: ID of the image being matched
                  - template_dimensions: dict with 'width' and 'height'
                  - screenshot_dimensions: dict with 'width' and 'height'
                  - similarity_threshold: threshold used (0.0-1.0)
                  - best_match_confidence: confidence of best match (0.0-1.0)
                  - threshold_passed: boolean indicating if match was successful
                  - best_match_location: dict with 'x' and 'y' coordinates (optional)

        Emits:
            IMAGE_RECOGNITION event with formatted data for frontend display

        Note:
            This method handles stdout restoration to prevent interference with
            qontinui's output capturing mechanism (via _capture_qontinui_output).

            All similarity values use the standard 0.0-1.0 range throughout the system.
        """
        # CRITICAL: Temporarily restore real stdout since callbacks may be called
        # while stdout is redirected to StringIO by _capture_qontinui_output()
        current_stdout = sys.stdout
        sys.stdout = self._real_stdout

        try:
            # Extract event data
            data = event.data
            template_dims = data.get("template_dimensions", {})
            screenshot_dims = data.get("screenshot_dimensions", {})
            match_loc = data.get("best_match_location", {})

            # Debug logging
            print(f"[EventTranslator] Processing MATCH_ATTEMPTED: image_id={data.get('image_id')}, threshold_passed={data.get('threshold_passed')}")

            # Get raw values from library (library provides 0.0-1.0 decimal format)
            threshold = data.get("similarity_threshold", 0)
            confidence = data.get("best_match_confidence", 0)
            print(f"[EventTranslator] Values from library: threshold={threshold}, confidence={confidence}")

            # Build frontend-compatible event data
            frontend_data = {
                "image_path": data.get("image_id", ""),
                # Format as "width, height" string for frontend display
                "template_size": f"{template_dims.get('width', 0)}, {template_dims.get('height', 0)}",
                "screenshot_size": f"{screenshot_dims.get('width', 0)}, {screenshot_dims.get('height', 0)}",
                # Pass through 0.0-1.0 values without modification
                # Frontend receives 0.0-1.0 and displays as percentages
                "threshold": threshold,
                "confidence": confidence,
                "found": data.get("threshold_passed", False),
                # Format location as "(x, y)" string if available
                "best_match_location": (
                    f"({match_loc.get('x', 0)}, {match_loc.get('y', 0)})"
                    if match_loc else None
                ),
                "event_type": "image_search",
                "raw_message": f"Match attempt: {data.get('image_id', 'unknown')}",
            }

            # Add gap and percent_off calculations for failed matches
            if not data.get("threshold_passed", False) and confidence > 0:
                frontend_data["gap"] = threshold - confidence
                frontend_data["percent_off"] = ((threshold - confidence) / threshold) if threshold > 0 else 0

            # Add state information if state_lookup callback is available
            if self.state_lookup:
                state_name = self.state_lookup(data.get("image_id", ""))
                if state_name:
                    frontend_data["state"] = state_name

            # Add hierarchy information if hierarchy_lookup callback is available
            if self.hierarchy_lookup:
                hierarchy = self.hierarchy_lookup()
                frontend_data["hierarchy"] = hierarchy

            # Add image data if image_data_lookup callback is available
            if self.image_data_lookup:
                image_id = data.get("image_id", "")
                image_data = self.image_data_lookup(image_id)
                if image_data:
                    frontend_data["image_data"] = image_data

            # Debug logging
            print(f"[EventTranslator] Emitting IMAGE_RECOGNITION: found={frontend_data['found']}, confidence={frontend_data['confidence']}")

            # Emit to frontend via the provided emitter callback
            self.emitter("image_recognition", frontend_data)

            print(f"[EventTranslator] IMAGE_RECOGNITION event emitted successfully")

        except Exception as e:
            # Log exception to help debug translation failures
            print(f"[EventTranslator] ERROR translating MATCH_ATTEMPTED: {e}")
            import traceback
            traceback.print_exc()
            # Re-raise so event registry can also log it
            raise

        finally:
            # Restore whatever stdout was active before callback
            sys.stdout = current_stdout

    # Future event handlers can be added below:

    def on_text_typed(self, event) -> None:
        """Translate text typing event to ACTION_EXECUTION format.

        Args:
            event: qontinui Event object with type=TEXT_TYPED
                  Expected data fields:
                  - text: The text that was typed
                  - length: Length of the text (optional)
                  - action_id: ID of the action (optional)
                  - success: Whether typing succeeded (optional)

        Emits:
            ACTION_EXECUTION event with typed text information
        """
        current_stdout = sys.stdout
        sys.stdout = self._real_stdout

        try:
            data = event.data

            frontend_data = {
                "action_type": "TYPE",
                "action_id": data.get("action_id", ""),
                "success": data.get("success", True),
                "attempts": 1,
                "config": {},
                "typed_text": data.get("text", ""),
                "raw_message": f"Typed text: '{data.get('text', '')}' ({data.get('length', 0)} chars)",
            }

            # Add hierarchy information if hierarchy_lookup callback is available
            if self.hierarchy_lookup:
                hierarchy = self.hierarchy_lookup()
                frontend_data["hierarchy"] = hierarchy
                print(f"[EventTranslator] TYPE action hierarchy: parent={hierarchy.get('parent_id')}, level={hierarchy.get('nesting_level')}, workflow={hierarchy.get('workflow_name')}")

            self.emitter("action_execution", frontend_data)

        finally:
            sys.stdout = current_stdout

    def on_mouse_clicked(self, event) -> None:
        """Translate mouse click event to ACTION_EXECUTION format.

        Args:
            event: qontinui Event object with type=MOUSE_CLICKED
                  Expected data fields:
                  - x: X coordinate of click
                  - y: Y coordinate of click
                  - button: Mouse button clicked ('left', 'right', 'middle')
                  - action_id: ID of the action (optional)
                  - success: Whether click succeeded

        Emits:
            ACTION_EXECUTION event with click location information

        Note:
            This is a placeholder for future implementation.
        """
        current_stdout = sys.stdout
        sys.stdout = self._real_stdout

        try:
            data = event.data

            frontend_data = {
                "action_type": "CLICK",
                "action_id": data.get("action_id", ""),
                "success": data.get("success", True),
                "attempts": 1,
                "config": {},
                "x": data.get("x", 0),
                "y": data.get("y", 0),
                "button": data.get("button", "left"),
                "raw_message": f"Clicked at ({data.get('x', 0)}, {data.get('y', 0)})",
            }

            # Add hierarchy information if hierarchy_lookup callback is available
            if self.hierarchy_lookup:
                hierarchy = self.hierarchy_lookup()
                frontend_data["hierarchy"] = hierarchy

            self.emitter("action_execution", frontend_data)

        finally:
            sys.stdout = current_stdout

    def on_action_started(self, event) -> None:
        """Translate action started event to frontend format.

        Args:
            event: qontinui Event object with type=ACTION_STARTED
                  Expected data fields:
                  - action_id: ID of the action
                  - action_type: Type of action (CLICK, TYPE, FIND, etc.)
                  - config: Action configuration

        Emits:
            ACTION_STARTED event for frontend tracking

        Note:
            This is a placeholder for future implementation.
        """
        current_stdout = sys.stdout
        sys.stdout = self._real_stdout

        try:
            data = event.data

            frontend_data = {
                "action_id": data.get("action_id", ""),
                "action_type": data.get("action_type", "UNKNOWN"),
            }

            # Add hierarchy information if hierarchy_lookup callback is available
            if self.hierarchy_lookup:
                hierarchy = self.hierarchy_lookup()
                frontend_data["hierarchy"] = hierarchy

            self.emitter("action_started", frontend_data)

        finally:
            sys.stdout = current_stdout

    def on_action_completed_library(self, event) -> None:
        """Handle library's ACTION_COMPLETED event and emit action_execution with hierarchy.

        This method intercepts ACTION_COMPLETED events from the library and translates
        them to action_execution events with proper hierarchy for frontend display.

        Args:
            event: qontinui Event object with type=ACTION_COMPLETED
                  Expected data fields:
                  - action_type: Type of action
                  - action_name: Name of action
                  - success: Whether action succeeded
                  - duration: Execution duration
                  - result: Action result data

        Emits:
            action_execution event with hierarchy for frontend tracking
        """
        current_stdout = sys.stdout
        sys.stdout = self._real_stdout

        try:
            data = event.data

            # Extract action info from library event
            action_type = getattr(data, "action_type", "UNKNOWN")
            success = getattr(data, "success", False)

            # Try to get action_id from result data if available
            result = getattr(data, "result", {})
            action_id = result.get("action_id", f"library-{action_type}-{id(event)}")

            frontend_data = {
                "action_type": action_type,
                "action_id": action_id,
                "success": success,
                "attempts": 1,  # Library doesn't provide this
                "config": {},
            }

            # Add action-specific data from result
            if hasattr(result, "__dict__"):
                result_dict = result.__dict__
            elif isinstance(result, dict):
                result_dict = result
            else:
                result_dict = {}

            # Add typed text for TYPE actions
            if action_type == "TYPE" and "text" in result_dict:
                frontend_data["typed_text"] = result_dict["text"]

            # Add error info if failed
            if not success:
                frontend_data["reason"] = "Action failed"
                if "error" in result_dict:
                    frontend_data["error"] = result_dict["error"]

            # Add hierarchy information if hierarchy_lookup callback is available
            if self.hierarchy_lookup:
                hierarchy = self.hierarchy_lookup()
                frontend_data["hierarchy"] = hierarchy
                print(f"[EventTranslator] Library action {action_type} completed with hierarchy: parent={hierarchy.get('parent_id')}, level={hierarchy.get('nesting_level')}, workflow={hierarchy.get('workflow_name')}")

            # Emit as action_execution event for frontend
            self.emitter("action_execution", frontend_data)

        except Exception as e:
            print(f"[EventTranslator] ERROR in on_action_completed_library: {e}")
            import traceback
            traceback.print_exc()

        finally:
            sys.stdout = current_stdout

    def on_action_completed(self, event) -> None:
        """Translate action completed event to frontend format.

        Args:
            event: qontinui Event object with type=ACTION_COMPLETED
                  Expected data fields:
                  - action_id: ID of the action
                  - success: Whether action succeeded
                  - error: Error message if failed (optional)

        Emits:
            ACTION_COMPLETED event for frontend tracking

        Note:
            This is a placeholder for future implementation.
        """
        current_stdout = sys.stdout
        sys.stdout = self._real_stdout

        try:
            data = event.data

            frontend_data = {
                "action_id": data.get("action_id", ""),
                "success": data.get("success", True),
            }

            if not data.get("success", True):
                frontend_data["error"] = data.get("error", "Unknown error")

            # Add hierarchy information if hierarchy_lookup callback is available
            if self.hierarchy_lookup:
                hierarchy = self.hierarchy_lookup()
                frontend_data["hierarchy"] = hierarchy

            self.emitter("action_completed", frontend_data)

        finally:
            sys.stdout = current_stdout


# Convenience function for creating and registering translator
def create_and_register_translator(
    emitter: Callable[[str, dict[str, Any]], None]
) -> EventTranslator:
    """Create an EventTranslator and register all callbacks.

    This is a convenience function that combines initialization and registration
    into a single call.

    Args:
        emitter: Callback function for emitting events to frontend

    Returns:
        Configured EventTranslator instance with all callbacks registered

    Example:
        >>> def my_emitter(event_type, data):
        ...     print(f"{event_type}: {data}")
        ...
        >>> translator = create_and_register_translator(my_emitter)
    """
    translator = EventTranslator(emitter)
    translator.register_all_callbacks()
    return translator
