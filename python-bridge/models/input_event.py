"""InputMonitorEvent data model for input monitoring and correlation with video frames.

This module provides the InputMonitorEvent dataclass for capturing all user input events
(mouse and keyboard) with precise timestamps for correlation with video frames.

Following the Single Responsibility Principle, this module:
- Defines the immutable InputMonitorEvent data structure
- Provides methods for converting to/from JSON
- Calculates frame numbers based on timestamps and FPS
- Does NOT capture or monitor input events
"""

from dataclasses import dataclass, field, asdict
from typing import Optional, Tuple, List, Dict, Any
import json


@dataclass
class InputMonitorEvent:
    """Immutable record of a single input event.

    This is the data structure for all input events captured during a session.
    Each event includes precise timing information for correlation with video frames.

    Timing Fields:
        timestamp: Unix timestamp in seconds with millisecond precision (float)
        frame_number: Calculated frame number: (timestamp - start_time) * fps

    Event Classification:
        event_type: Type of input event
            - "mouse_click": Mouse button press and release
            - "mouse_drag": Mouse button held while moving
            - "mouse_scroll": Mouse wheel scroll
            - "mouse_move": Cursor movement (sampled)
            - "key_press": Keyboard key press
            - "key_release": Keyboard key release
            - "key_combo": Multiple keys pressed together

    Mouse Event Fields:
        x: X-coordinate of mouse position (None for keyboard events)
        y: Y-coordinate of mouse position (None for keyboard events)
        button: Mouse button ("left", "right", "middle", None for non-click events)
        scroll_delta: Scroll amount (positive=up, negative=down, None for non-scroll)
        drag_start: Starting position of drag as (x, y) tuple
        drag_end: Ending position of drag as (x, y) tuple

    Keyboard Event Fields:
        key: Human-readable key name (e.g., "a", "enter", "space")
        key_code: Numeric key code if available
        modifiers: List of modifier keys held during event (e.g., ["ctrl", "shift"])

    Additional Fields:
        duration: Duration of the event in milliseconds (e.g., click duration, key hold time)
        metadata: Additional arbitrary data for extensibility
    """

    # Timing
    timestamp: float
    frame_number: int

    # Event classification
    event_type: str

    # Mouse event fields
    x: Optional[int] = None
    y: Optional[int] = None
    button: Optional[str] = None
    scroll_delta: Optional[int] = None
    drag_start: Optional[Tuple[int, int]] = None
    drag_end: Optional[Tuple[int, int]] = None

    # Keyboard event fields
    key: Optional[str] = None
    key_code: Optional[int] = None
    modifiers: List[str] = field(default_factory=list)

    # Additional fields
    duration: Optional[float] = None  # Duration in milliseconds
    metadata: Dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> Dict[str, Any]:
        """Convert the InputMonitorEvent to a dictionary.

        This method converts the dataclass to a dictionary suitable for JSON
        serialization. It handles tuple conversion for drag coordinates.

        Returns:
            Dictionary representation of the event
        """
        event_dict = asdict(self)

        # Convert tuples to lists for JSON serialization
        if self.drag_start is not None:
            event_dict["drag_start"] = list(self.drag_start)
        if self.drag_end is not None:
            event_dict["drag_end"] = list(self.drag_end)

        return event_dict

    def to_json(self) -> str:
        """Convert the InputMonitorEvent to a JSON string.

        Returns:
            JSON string representation of the event
        """
        return json.dumps(self.to_dict())

    def to_jsonl(self) -> str:
        """Convert the InputMonitorEvent to a JSONL line.

        JSONL (JSON Lines) format is one JSON object per line, suitable for
        streaming and appending to log files.

        Returns:
            JSONL line (JSON string without newline)
        """
        return json.dumps(self.to_dict(), separators=(",", ":"))

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> "InputMonitorEvent":
        """Create an InputMonitorEvent from a dictionary.

        Args:
            data: Dictionary containing event data

        Returns:
            InputMonitorEvent instance
        """
        # Convert lists back to tuples for drag coordinates
        if "drag_start" in data and data["drag_start"] is not None:
            data["drag_start"] = tuple(data["drag_start"])
        if "drag_end" in data and data["drag_end"] is not None:
            data["drag_end"] = tuple(data["drag_end"])

        return cls(**data)

    @classmethod
    def from_json(cls, json_str: str) -> "InputMonitorEvent":
        """Create an InputMonitorEvent from a JSON string.

        Args:
            json_str: JSON string representation

        Returns:
            InputMonitorEvent instance
        """
        data = json.loads(json_str)
        return cls.from_dict(data)

    @staticmethod
    def calculate_frame_number(timestamp: float, start_time: float, fps: int) -> int:
        """Calculate the frame number for a given timestamp.

        This is a utility method for calculating frame numbers during event capture.

        Args:
            timestamp: Event timestamp (Unix timestamp in seconds)
            start_time: Session start time (Unix timestamp in seconds)
            fps: Frames per second of the video recording

        Returns:
            Calculated frame number (0-based)
        """
        elapsed = timestamp - start_time
        return int(elapsed * fps)
