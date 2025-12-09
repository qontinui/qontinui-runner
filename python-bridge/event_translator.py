#!/usr/bin/env python3
"""
Event translator for converting qontinui library events to frontend JSON format.

This module provides the EventTranslator class that registers callbacks with the
qontinui library's event system and translates them into the format expected by
the Tauri frontend.
"""

import base64
import sys
from collections.abc import Callable
from typing import Any

try:
    from qontinui.reporting import EventType as QontinuiEventType
    from qontinui.reporting import register_callback

    QONTINUI_AVAILABLE = True
except ImportError:
    QONTINUI_AVAILABLE = False
    QontinuiEventType = None

from services.unified_data_collector import UnifiedDataCollector


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

    def __init__(
        self,
        emitter: Callable[[str, dict[str, Any]], None],
        state_lookup: Callable[[str], str | None] | None = None,
        hierarchy_lookup: Callable[[], dict[str, Any]] | None = None,
        image_data_lookup: Callable[[str], str | None] | None = None,
        collector: UnifiedDataCollector | None = None,
    ):
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
            collector: Optional UnifiedDataCollector for recording execution data.
                      When provided, events are recorded to both the collector (for
                      execution records) and emitted to the frontend (for real-time display).

        Note:
            The emitter should handle converting the event_type string to the
            appropriate EventType enum value if needed.
        """
        self.emitter = emitter
        self.state_lookup = state_lookup
        self.hierarchy_lookup = hierarchy_lookup
        self.image_data_lookup = image_data_lookup
        self.collector = collector
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
        print("[EventTranslator] register_all_callbacks() starting...", file=sys.stderr, flush=True)

        # Register MATCH_ATTEMPTED callback for image recognition events
        print(
            "[EventTranslator] Registering MATCH_ATTEMPTED callback...", file=sys.stderr, flush=True
        )
        register_callback(QontinuiEventType.MATCH_ATTEMPTED, self.on_match_attempted)
        print("[EventTranslator] MATCH_ATTEMPTED callback registered", file=sys.stderr, flush=True)

        # Register TEXT_TYPED callback for typing action events
        print("[EventTranslator] Registering TEXT_TYPED callback...", file=sys.stderr, flush=True)
        register_callback(QontinuiEventType.TEXT_TYPED, self.on_text_typed)
        print("[EventTranslator] TEXT_TYPED callback registered", file=sys.stderr, flush=True)

        # Register ACTION_COMPLETED callback to add hierarchy to library action events
        print(
            "[EventTranslator] Registering ACTION_COMPLETED callback...",
            file=sys.stderr,
            flush=True,
        )
        register_callback(QontinuiEventType.ACTION_COMPLETED, self.on_action_completed_library)
        print("[EventTranslator] ACTION_COMPLETED callback registered", file=sys.stderr, flush=True)

        print("[EventTranslator] register_all_callbacks() completed", file=sys.stderr, flush=True)

        # Future: Additional callbacks can be registered here
        # register_callback(QontinuiEventType.ACTION_STARTED, self.on_action_started)
        # register_callback(QontinuiEventType.MOUSE_CLICKED, self.on_mouse_clicked)

    def on_match_attempted(self, event) -> None:
        """Translate MATCH_ATTEMPTED event to IMAGE_RECOGNITION format.

        Converts qontinui library's MATCH_ATTEMPTED event into the frontend's
        IMAGE_RECOGNITION event format with properly formatted data fields.
        Also records match data to UnifiedDataCollector if available.

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
                  - screenshot_base64: Optional base64-encoded screenshot
                  - debug_visual_base64: Optional base64-encoded debug visualization

        Emits:
            IMAGE_RECOGNITION event with formatted data for frontend display

        Note:
            This method handles stdout restoration to prevent interference with
            qontinui's output capturing mechanism (via _capture_qontinui_output).

            All similarity values use the standard 0.0-1.0 range throughout the system.

            When collector is available, operates in dual mode:
            1. Records match data to collector for execution records
            2. Emits events to frontend for real-time display
        """
        # CRITICAL: Temporarily restore real stdout since callbacks may be called
        # while stdout is redirected to StringIO by _capture_qontinui_output()
        current_stdout = sys.stdout
        sys.stdout = self._real_stdout

        try:
            # PROMINENT DEBUG LOGGING - Verify callback is being called
            print(f"\n{'='*80}", file=sys.stderr, flush=True)
            print(
                "[EventTranslator] !!!!! MATCH_ATTEMPTED EVENT RECEIVED !!!!!",
                file=sys.stderr,
                flush=True,
            )
            print(f"{'='*80}\n", file=sys.stderr, flush=True)

            # File-based debug logging (works even when stderr is disabled)
            import os
            import tempfile
            from datetime import datetime

            debug_log = os.path.join(tempfile.gettempdir(), "qontinui_event_emission.log")
            try:
                with open(debug_log, "a", encoding="utf-8") as f:
                    ts = datetime.now().strftime("%Y-%m-%d %H:%M:%S.%f")[:-3]
                    f.write(f"[{ts}] EventTranslator.on_match_attempted() CALLED\n")
                    f.write(f"[{ts}]   event={event}\n")
                    f.write(f"[{ts}]   event.data={event.data}\n")
            except Exception:
                pass

            # Extract event data
            data = event.data

            # Handle both old and new format
            # NEW format (from real_find_implementation.py): threshold, confidence, found, location, template_size
            # OLD format: similarity_threshold, best_match_confidence, threshold_passed, best_match_location, template_dimensions

            # Get template and screenshot dimensions
            template_size = data.get("template_size", (0, 0))
            if isinstance(template_size, tuple):
                template_dims = {"width": template_size[0], "height": template_size[1]}
            else:
                template_dims = data.get("template_dimensions", {})

            screenshot_dims = data.get("screenshot_dimensions", {})

            # Get match location (NEW format uses dict with x,y,width,height, OLD uses best_match_location dict)
            location_data = data.get("location")
            if location_data:
                if isinstance(location_data, dict):
                    # NEW format: dict with x, y, width, height
                    match_loc = location_data
                elif isinstance(location_data, tuple):
                    # Fallback: tuple with (x, y) only
                    match_loc = {
                        "x": location_data[0],
                        "y": location_data[1],
                        "width": 0,
                        "height": 0,
                    }
                else:
                    match_loc = {}
            else:
                # OLD format
                match_loc = data.get("best_match_location", {})

            # Debug logging
            print(
                f"[EventTranslator] Processing MATCH_ATTEMPTED: image_id={data.get('image_id')}, found={data.get('found')}",
                file=sys.stderr,
            )

            # Get threshold and confidence (NEW format: threshold/confidence, OLD format: similarity_threshold/best_match_confidence)
            threshold = data.get("threshold", data.get("similarity_threshold", 0))
            confidence = data.get("confidence", data.get("best_match_confidence", 0))
            found = data.get("found", data.get("threshold_passed", False))

            print(
                f"[EventTranslator] Values from library: threshold={threshold}, confidence={confidence}, found={found}",
                file=sys.stderr,
            )

            # --- INTEGRATION: Record match data to UnifiedDataCollector ---
            # When collector is available, record match result for execution tracking.
            # This enables dual mode: collector gets structured data for records,
            # frontend gets formatted events for real-time display.
            screenshot_path = None
            debug_visual_path = None
            if self.collector:
                # Extract lightweight match summary for collector
                match_summary = {
                    "image_id": data.get("image_id", ""),
                    "found": found,
                    "confidence": confidence,
                    "threshold": threshold,
                    "location": (
                        {"x": match_loc.get("x", 0), "y": match_loc.get("y", 0)}
                        if match_loc
                        else None
                    ),
                    "method": data.get("method", "template_matching"),
                    "template_size": {
                        "width": template_dims.get("width", 0),
                        "height": template_dims.get("height", 0),
                    },
                    "screenshot_size": {
                        "width": screenshot_dims.get("width", 0),
                        "height": screenshot_dims.get("height", 0),
                    },
                    "search_region": data.get(
                        "search_region"
                    ),  # Optional: region where search was performed
                }

                # Decode screenshot data if available (base64 -> bytes)
                screenshot_data = None
                screenshot_base64 = data.get("screenshot_base64")
                if screenshot_base64:
                    try:
                        screenshot_data = base64.b64decode(screenshot_base64)
                    except Exception as e:
                        print(
                            f"[EventTranslator] Failed to decode screenshot_base64: {e}",
                            file=sys.stderr,
                        )

                # Decode debug visual data if available (base64 -> bytes)
                # NEW: Library sends visual_debug_image, OLD: debug_visual_base64
                debug_visual_data = None
                debug_visual_base64 = data.get(
                    "visual_debug_image", data.get("debug_visual_base64")
                )
                if debug_visual_base64:
                    try:
                        debug_visual_data = base64.b64decode(debug_visual_base64)
                        print(
                            f"[EventTranslator] Decoded visual debug image, size={len(debug_visual_data)} bytes",
                            file=sys.stderr,
                        )
                    except Exception as e:
                        print(
                            f"[EventTranslator] Failed to decode visual_debug_image: {e}",
                            file=sys.stderr,
                        )

                # Record to collector (thread-safe) and get saved file paths
                try:
                    paths = self.collector.record_match_result(
                        match_summary=match_summary,
                        screenshot_data=screenshot_data,
                        debug_visual_data=debug_visual_data,
                    )
                    screenshot_path = paths.get("screenshot_path")
                    debug_visual_path = paths.get("debug_visual_path")
                    print(
                        f"[EventTranslator] Match result recorded to collector: found={match_summary['found']}, screenshot={screenshot_path is not None}",
                        file=sys.stderr,
                    )
                except Exception as e:
                    print(
                        f"[EventTranslator] Failed to record match to collector: {e}",
                        file=sys.stderr,
                    )
                    import traceback

                    traceback.print_exc()

            # --- EXISTING: Build frontend-compatible event data ---
            frontend_data = {
                "image_path": data.get(
                    "pattern_name", data.get("image_path", data.get("image_id", ""))
                ),
                "template_name": data.get(
                    "pattern_name", data.get("image_path", data.get("image_id", ""))
                ),
                # Format as "width, height" string for frontend display
                "template_size": f"{template_dims.get('width', 0)}, {template_dims.get('height', 0)}",
                "screenshot_size": f"{screenshot_dims.get('width', 0)}, {screenshot_dims.get('height', 0)}",
                # Pass through 0.0-1.0 values without modification
                # Frontend receives 0.0-1.0 and displays as percentages
                "threshold": threshold,
                "confidence": confidence,
                "found": found,
                # Send location as object for frontend (used when match found)
                "location": (
                    {
                        "x": match_loc.get("x", 0),
                        "y": match_loc.get("y", 0),
                        "width": match_loc.get("width", 0),
                        "height": match_loc.get("height", 0),
                    }
                    if match_loc
                    else None
                ),
                # Also send as string for convenience (used when match not found)
                "best_match_location": (
                    f"({match_loc.get('x', 0)}, {match_loc.get('y', 0)})" if match_loc else None
                ),
                "event_type": "image_search",
                "raw_message": f"Match attempt: {data.get('image_id', 'unknown')}",
            }

            # Pass through image_data from library if available (base64 template image)
            if data.get("image_data"):
                frontend_data["image_data"] = data.get("image_data")

            # Pass through template_image if available (alias for image_data)
            if data.get("template_image") and not frontend_data.get("image_data"):
                frontend_data["image_data"] = data.get("template_image")

            # Add gap and percent_off calculations for failed matches
            if not found and confidence > 0:
                frontend_data["gap"] = threshold - confidence
                frontend_data["percent_off"] = (
                    ((threshold - confidence) / threshold) if threshold > 0 else 0
                )

            # Add state information if state_lookup callback is available
            if self.state_lookup:
                state_name = self.state_lookup(data.get("image_id", ""))
                if state_name:
                    frontend_data["state"] = state_name

            # Add hierarchy information if hierarchy_lookup callback is available
            if self.hierarchy_lookup:
                hierarchy = self.hierarchy_lookup()
                frontend_data["hierarchy"] = hierarchy

            # Add image data if image_data_lookup callback is available (only if not already set from library)
            if self.image_data_lookup and not frontend_data.get("image_data"):
                image_id = data.get("image_id", "")
                image_data = self.image_data_lookup(image_id)
                if image_data:
                    frontend_data["image_data"] = image_data

            # Add screenshot and debug visual paths if available from collector
            if screenshot_path:
                frontend_data["screenshot_path"] = screenshot_path
            if debug_visual_path:
                frontend_data["template_path"] = (
                    debug_visual_path  # Use as template path for visualization
                )

            # Pass through screenshot_base64 from library (for display when no file path)
            if data.get("screenshot_base64"):
                frontend_data["screenshot_base64"] = data.get("screenshot_base64")
                print(
                    f"[EventTranslator] Including screenshot_base64 (length={len(data.get('screenshot_base64'))})",
                    file=sys.stderr,
                )

            # Pass through visual_debug_image (annotated screenshot with colored boxes)
            if data.get("visual_debug_image"):
                frontend_data["visual_debug_image"] = data.get("visual_debug_image")
                print(
                    f"[EventTranslator] Including visual_debug_image (length={len(data.get('visual_debug_image'))})",
                    file=sys.stderr,
                )

            # Pass through matched_region_image (cropped region from screenshot at match location)
            if data.get("matched_region_image"):
                frontend_data["matched_region_image"] = data.get("matched_region_image")
                print(
                    f"[EventTranslator] Including matched_region_image (length={len(data.get('matched_region_image'))})",
                    file=sys.stderr,
                )

            # Pass through debug data for match details
            if data.get("debug"):
                frontend_data["debug"] = data.get("debug")
                print("[EventTranslator] Including debug data", file=sys.stderr)

            # Debug logging
            print(
                f"[EventTranslator] Emitting IMAGE_RECOGNITION: found={frontend_data['found']}, confidence={frontend_data['confidence']}, screenshot_path={screenshot_path is not None}, screenshot_base64={data.get('screenshot_base64') is not None}, visual_debug={data.get('visual_debug_image') is not None}",
                file=sys.stderr,
            )

            # Emit to frontend via the provided emitter callback
            self.emitter("image_recognition", frontend_data)

            print("[EventTranslator] IMAGE_RECOGNITION event emitted successfully", file=sys.stderr)

        except Exception as e:
            # Log exception to help debug translation failures
            print(f"[EventTranslator] ERROR translating MATCH_ATTEMPTED: {e}", file=sys.stderr)
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

        Records typed text to UnifiedDataCollector if available, then emits
        to frontend for real-time display.

        Args:
            event: qontinui Event object with type=TEXT_TYPED
                  Expected data fields:
                  - text: The text that was typed
                  - length: Length of the text (optional)
                  - action_id: ID of the action (optional)
                  - success: Whether typing succeeded (optional)

        Emits:
            ACTION_EXECUTION event with typed text information

        Note:
            When collector is available, operates in dual mode:
            1. Records text to collector for execution records
            2. Emits events to frontend for real-time display
        """
        current_stdout = sys.stdout
        sys.stdout = self._real_stdout

        print("[EventTranslator] on_text_typed() called!", file=sys.stderr, flush=True)

        try:
            data = event.data
            text = data.get("text", "")
            print(
                f"[EventTranslator] TEXT_TYPED event data: text='{text}'",
                file=sys.stderr,
                flush=True,
            )

            # --- INTEGRATION: Record text typed to UnifiedDataCollector ---
            # When collector is available, record the typed text for execution tracking.
            # This enables dual mode: collector gets data for records,
            # frontend gets events for real-time display.
            if self.collector and text:
                try:
                    self.collector.record_text_typed(text)
                    print(
                        f"[EventTranslator] Typed text recorded to collector: '{text}' ({len(text)} chars)",
                        file=sys.stderr,
                        flush=True,
                    )
                except Exception as e:
                    print(
                        f"[EventTranslator] Failed to record text to collector: {e}",
                        file=sys.stderr,
                        flush=True,
                    )
                    import traceback

                    traceback.print_exc()

            # --- EXISTING: Build frontend-compatible event data ---
            frontend_data = {
                "action_type": "TYPE",
                "action_id": data.get("action_id", ""),
                "success": data.get("success", True),
                "attempts": 1,
                "config": {},
                "typed_text": text,
                "raw_message": f"Typed text: '{text}' ({data.get('length', 0)} chars)",
            }

            # Add hierarchy information if hierarchy_lookup callback is available
            if self.hierarchy_lookup:
                hierarchy = self.hierarchy_lookup()
                frontend_data["hierarchy"] = hierarchy
                print(
                    f"[EventTranslator] TYPE action hierarchy: parent={hierarchy.get('parent_id')}, level={hierarchy.get('nesting_level')}, workflow={hierarchy.get('workflow_name')}",
                    file=sys.stderr,
                    flush=True,
                )

            print(
                "[EventTranslator] Emitting action_execution event for TYPE action",
                file=sys.stderr,
                flush=True,
            )
            self.emitter("action_execution", frontend_data)
            print(
                "[EventTranslator] action_execution event emitted successfully",
                file=sys.stderr,
                flush=True,
            )

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
                print(
                    f"[EventTranslator] Library action {action_type} completed with hierarchy: parent={hierarchy.get('parent_id')}, level={hierarchy.get('nesting_level')}, workflow={hierarchy.get('workflow_name')}",
                    file=sys.stderr,
                )

            # Emit as action_execution event for frontend
            self.emitter("action_execution", frontend_data)

        except Exception as e:
            print(f"[EventTranslator] ERROR in on_action_completed_library: {e}", file=sys.stderr)
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
    emitter: Callable[[str, dict[str, Any]], None],
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
