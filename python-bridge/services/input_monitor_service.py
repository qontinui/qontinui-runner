"""Input Monitor Service Module.

This module implements the InputMonitorService class, which captures all user input
events (mouse and keyboard) with precise timestamps for correlation with video frames.

The service follows the Single Responsibility Principle (SRP) by focusing solely on
input event capture and storage. It handles:
- Mouse events (clicks, drags, scrolls, moves)
- Keyboard events (key presses, releases, combos)
- Event timestamping with millisecond precision
- Frame number calculation for video correlation
- JSONL streaming storage for efficient writes

The service is designed to be:
- Cross-platform: Uses pynput for OS-independent input monitoring
- High-precision: Millisecond-level timestamps
- Non-blocking: Events captured in background thread
- Streaming: JSONL format for efficient append operations
- Thread-safe: Proper locking for concurrent access
"""

import json
import logging
import threading
import time
from pathlib import Path
from typing import List, Optional, Tuple, Dict, Any
from dataclasses import replace

try:
    from pynput import mouse, keyboard

    PYNPUT_AVAILABLE = True
except ImportError:
    PYNPUT_AVAILABLE = False
    logging.warning("pynput library not available. InputMonitorService will not function.")

from models.input_event import InputMonitorEvent


# Configure logging
logger = logging.getLogger(__name__)


class InputMonitorService:
    """Service for monitoring and recording all user input events.

    This service captures mouse and keyboard events in real-time with precise
    timestamps for correlation with video frames. Events are stored in JSONL
    format for efficient streaming and processing.

    Responsibilities:
    1. Monitor mouse events (clicks, drags, scrolls, moves)
    2. Monitor keyboard events (key presses, releases, combos)
    3. Calculate frame numbers based on timestamps and FPS
    4. Store events in JSONL format for streaming
    5. Provide event retrieval and filtering capabilities

    Thread Safety:
        This service uses threading locks to ensure thread-safe access to
        the events list and file operations.

    Usage Example:
        >>> service = InputMonitorService(storage_dir=Path("/tmp/sessions"))
        >>> service.start_monitoring(session_id="session-123", fps=30)
        >>> # ... user performs actions ...
        >>> events_file = service.stop_monitoring()
        >>> events = service.get_events()
        >>> print(f"Captured {len(events)} input events")
    """

    def __init__(self, storage_dir: Path) -> None:
        """Initialize the InputMonitorService.

        Args:
            storage_dir: Root directory for storing input event files.
                        Events will be stored in storage_dir/input_events/

        Raises:
            ImportError: If pynput library is not available
        """
        if not PYNPUT_AVAILABLE:
            raise ImportError(
                "pynput library is required for InputMonitorService. "
                "Install it with: pip install pynput"
            )

        self.storage_dir = storage_dir
        self.input_events_dir = storage_dir / "input_events"
        self.input_events_dir.mkdir(parents=True, exist_ok=True)

        # Monitoring state
        self._monitoring = False
        self._session_id: Optional[str] = None
        self._start_time: Optional[float] = None
        self._fps: int = 30
        self._events: List[InputMonitorEvent] = []
        self._events_file: Optional[Path] = None
        self._events_lock = threading.Lock()

        # Mouse state tracking
        self._mouse_pressed_button: Optional[str] = None
        self._mouse_press_time: Optional[float] = None
        self._mouse_press_position: Optional[Tuple[int, int]] = None
        self._is_dragging = False
        self._drag_positions: List[Tuple[int, int]] = []
        self._last_mouse_move_time: float = 0
        self._mouse_move_sample_interval = 0.1  # Sample mouse moves every 100ms

        # Keyboard state tracking
        self._keys_pressed: Dict[str, float] = {}  # key -> press timestamp
        self._current_modifiers: List[str] = []

        # pynput listeners
        self._mouse_listener: Optional[mouse.Listener] = None
        self._keyboard_listener: Optional[keyboard.Listener] = None

        logger.info(f"InputMonitorService initialized with storage_dir: {storage_dir}")

    def start_monitoring(self, session_id: str, fps: int = 30) -> bool:
        """Start monitoring input events for a session.

        This method initializes the input monitoring system and starts capturing
        all mouse and keyboard events. Events are written to a JSONL file in
        real-time for streaming compatibility.

        Args:
            session_id: Unique identifier for this monitoring session
            fps: Frames per second for video recording (used to calculate frame numbers)

        Returns:
            True if monitoring started successfully, False otherwise

        Raises:
            RuntimeError: If monitoring is already active
        """
        if self._monitoring:
            raise RuntimeError(
                f"Monitoring already active for session: {self._session_id}. "
                "Stop current monitoring before starting a new session."
            )

        try:
            self._session_id = session_id
            self._start_time = time.time()
            self._fps = fps
            self._events.clear()
            self._monitoring = True

            # Initialize events file
            self._events_file = self.input_events_dir / f"{session_id}_events.jsonl"
            self._events_file.touch()  # Create empty file

            # Reset state
            self._mouse_pressed_button = None
            self._mouse_press_time = None
            self._mouse_press_position = None
            self._is_dragging = False
            self._drag_positions.clear()
            self._last_mouse_move_time = 0
            self._keys_pressed.clear()
            self._current_modifiers.clear()

            # Start pynput listeners
            self._mouse_listener = mouse.Listener(
                on_move=self._on_mouse_move,
                on_click=self._on_mouse_click,
                on_scroll=self._on_mouse_scroll,
            )
            self._keyboard_listener = keyboard.Listener(
                on_press=self._on_key_press, on_release=self._on_key_release
            )

            self._mouse_listener.start()
            self._keyboard_listener.start()

            logger.info(
                f"Input monitoring started for session: {session_id}, "
                f"fps: {fps}, events_file: {self._events_file}"
            )
            return True

        except Exception as e:
            logger.error(f"Failed to start input monitoring: {e}", exc_info=True)
            self._monitoring = False
            return False

    def stop_monitoring(self) -> str:
        """Stop monitoring input events and return the events file path.

        This method stops all input listeners and finalizes the events file.

        Returns:
            Absolute path to the events file (JSONL format)

        Raises:
            RuntimeError: If monitoring is not active
        """
        if not self._monitoring:
            raise RuntimeError("Monitoring is not active. Start monitoring before stopping.")

        try:
            # Stop listeners
            if self._mouse_listener:
                self._mouse_listener.stop()
                self._mouse_listener = None

            if self._keyboard_listener:
                self._keyboard_listener.stop()
                self._keyboard_listener = None

            # Finalize any pending drag events
            if self._is_dragging and self._mouse_press_position:
                self._finalize_drag_event()

            self._monitoring = False

            events_file_path = str(self._events_file.absolute())
            event_count = len(self._events)

            logger.info(
                f"Input monitoring stopped for session: {self._session_id}, "
                f"captured {event_count} events, file: {events_file_path}"
            )

            return events_file_path

        except Exception as e:
            logger.error(f"Error stopping input monitoring: {e}", exc_info=True)
            self._monitoring = False
            raise

    def get_events(self) -> List[InputMonitorEvent]:
        """Get all captured input events.

        Returns:
            List of all InputMonitorEvent objects captured during this session
        """
        with self._events_lock:
            return self._events.copy()

    def get_events_in_range(self, start_time: float, end_time: float) -> List[InputMonitorEvent]:
        """Get input events within a specific time range.

        Args:
            start_time: Start of time range (Unix timestamp)
            end_time: End of time range (Unix timestamp)

        Returns:
            List of InputMonitorEvent objects within the specified time range
        """
        with self._events_lock:
            return [event for event in self._events if start_time <= event.timestamp <= end_time]

    def _add_event(self, event: InputMonitorEvent) -> None:
        """Add an event to the events list and write to JSONL file.

        This is an internal method that handles thread-safe event storage.

        Args:
            event: InputMonitorEvent to add
        """
        with self._events_lock:
            self._events.append(event)

            # Write to JSONL file
            if self._events_file:
                try:
                    with open(self._events_file, "a", encoding="utf-8") as f:
                        f.write(event.to_jsonl() + "\n")
                except Exception as e:
                    logger.error(f"Failed to write event to file: {e}", exc_info=True)

    def _calculate_frame_number(self, timestamp: float) -> int:
        """Calculate frame number for a given timestamp.

        Args:
            timestamp: Event timestamp

        Returns:
            Calculated frame number
        """
        if self._start_time is None:
            return 0
        return InputMonitorEvent.calculate_frame_number(timestamp, self._start_time, self._fps)

    # Mouse event handlers

    def _on_mouse_move(self, x: int, y: int) -> None:
        """Handle mouse move events.

        Mouse moves are sampled at intervals to avoid excessive event volume.
        If dragging, positions are tracked for drag event creation.

        Args:
            x: X-coordinate of mouse position
            y: Y-coordinate of mouse position
        """
        if not self._monitoring:
            return

        current_time = time.time()

        # Track drag positions
        if self._is_dragging:
            self._drag_positions.append((x, y))

        # Sample mouse moves at intervals to avoid excessive events
        if current_time - self._last_mouse_move_time >= self._mouse_move_sample_interval:
            timestamp = current_time
            frame_number = self._calculate_frame_number(timestamp)

            event = InputMonitorEvent(
                timestamp=timestamp, frame_number=frame_number, event_type="mouse_move", x=x, y=y
            )
            self._add_event(event)
            self._last_mouse_move_time = current_time

    def _on_mouse_click(self, x: int, y: int, button: mouse.Button, pressed: bool) -> None:
        """Handle mouse click events (both press and release).

        This method tracks button press/release pairs to create click events
        with duration. It also detects drag operations.

        Args:
            x: X-coordinate of click
            y: Y-coordinate of click
            button: Mouse button that was clicked
            pressed: True if button was pressed, False if released
        """
        if not self._monitoring:
            return

        timestamp = time.time()
        button_name = self._get_button_name(button)

        if pressed:
            # Button press - start tracking
            self._mouse_pressed_button = button_name
            self._mouse_press_time = timestamp
            self._mouse_press_position = (x, y)
            self._is_dragging = False
            self._drag_positions = [(x, y)]

        else:
            # Button release
            if self._mouse_press_time and self._mouse_press_position:
                duration = (timestamp - self._mouse_press_time) * 1000  # Convert to ms

                # Check if this was a drag (moved while button held)
                if self._is_dragging or (
                    len(self._drag_positions) > 1
                    and self._has_significant_movement(self._mouse_press_position, (x, y))
                ):
                    # Create drag event
                    self._finalize_drag_event(end_position=(x, y), end_time=timestamp)
                else:
                    # Create click event
                    frame_number = self._calculate_frame_number(timestamp)
                    event = InputMonitorEvent(
                        timestamp=timestamp,
                        frame_number=frame_number,
                        event_type="mouse_click",
                        x=x,
                        y=y,
                        button=button_name,
                        duration=duration,
                    )
                    self._add_event(event)

                # Reset state
                self._mouse_pressed_button = None
                self._mouse_press_time = None
                self._mouse_press_position = None
                self._is_dragging = False
                self._drag_positions.clear()

    def _on_mouse_scroll(self, x: int, y: int, dx: int, dy: int) -> None:
        """Handle mouse scroll events.

        Args:
            x: X-coordinate of mouse position during scroll
            y: Y-coordinate of mouse position during scroll
            dx: Horizontal scroll delta
            dy: Vertical scroll delta
        """
        if not self._monitoring:
            return

        timestamp = time.time()
        frame_number = self._calculate_frame_number(timestamp)

        # Use vertical scroll delta (dy) as the primary scroll value
        event = InputMonitorEvent(
            timestamp=timestamp,
            frame_number=frame_number,
            event_type="mouse_scroll",
            x=x,
            y=y,
            scroll_delta=int(dy),
            metadata={"horizontal_delta": int(dx)},
        )
        self._add_event(event)

    def _finalize_drag_event(
        self, end_position: Optional[Tuple[int, int]] = None, end_time: Optional[float] = None
    ) -> None:
        """Finalize and record a drag event.

        Args:
            end_position: Ending position of drag (uses last tracked position if None)
            end_time: End timestamp (uses current time if None)
        """
        if not self._mouse_press_position or not self._mouse_press_time:
            return

        timestamp = end_time if end_time else time.time()
        frame_number = self._calculate_frame_number(timestamp)
        duration = (timestamp - self._mouse_press_time) * 1000  # Convert to ms

        end_pos = (
            end_position
            if end_position
            else (self._drag_positions[-1] if self._drag_positions else self._mouse_press_position)
        )

        event = InputMonitorEvent(
            timestamp=timestamp,
            frame_number=frame_number,
            event_type="mouse_drag",
            x=end_pos[0],
            y=end_pos[1],
            button=self._mouse_pressed_button,
            drag_start=self._mouse_press_position,
            drag_end=end_pos,
            duration=duration,
            metadata={"path_length": len(self._drag_positions)},
        )
        self._add_event(event)

    # Keyboard event handlers

    def _on_key_press(self, key) -> None:
        """Handle keyboard key press events.

        Args:
            key: Key object from pynput
        """
        if not self._monitoring:
            return

        timestamp = time.time()
        frame_number = self._calculate_frame_number(timestamp)
        key_str = self._get_key_string(key)

        # Track modifier keys
        if self._is_modifier_key(key_str):
            if key_str not in self._current_modifiers:
                self._current_modifiers.append(key_str)

        # Track pressed keys for duration calculation
        self._keys_pressed[key_str] = timestamp

        event = InputMonitorEvent(
            timestamp=timestamp,
            frame_number=frame_number,
            event_type="key_press",
            key=key_str,
            key_code=self._get_key_code(key),
            modifiers=self._current_modifiers.copy(),
        )
        self._add_event(event)

    def _on_key_release(self, key) -> None:
        """Handle keyboard key release events.

        Args:
            key: Key object from pynput
        """
        if not self._monitoring:
            return

        timestamp = time.time()
        frame_number = self._calculate_frame_number(timestamp)
        key_str = self._get_key_string(key)

        # Calculate duration if we have the press time
        duration = None
        if key_str in self._keys_pressed:
            press_time = self._keys_pressed.pop(key_str)
            duration = (timestamp - press_time) * 1000  # Convert to ms

        # Remove from modifiers if it's a modifier key
        if key_str in self._current_modifiers:
            self._current_modifiers.remove(key_str)

        event = InputMonitorEvent(
            timestamp=timestamp,
            frame_number=frame_number,
            event_type="key_release",
            key=key_str,
            key_code=self._get_key_code(key),
            modifiers=self._current_modifiers.copy(),
            duration=duration,
        )
        self._add_event(event)

    # Helper methods

    @staticmethod
    def _get_button_name(button: mouse.Button) -> str:
        """Convert pynput button to string name.

        Args:
            button: Mouse button from pynput

        Returns:
            Button name string ("left", "right", "middle", or "unknown")
        """
        if button == mouse.Button.left:
            return "left"
        elif button == mouse.Button.right:
            return "right"
        elif button == mouse.Button.middle:
            return "middle"
        else:
            return "unknown"

    @staticmethod
    def _get_key_string(key) -> str:
        """Convert pynput key to string representation.

        Args:
            key: Key object from pynput

        Returns:
            String representation of the key
        """
        try:
            if hasattr(key, "char") and key.char is not None:
                return key.char
            elif hasattr(key, "name"):
                return key.name
            else:
                return str(key)
        except Exception:
            return str(key)

    @staticmethod
    def _get_key_code(key) -> Optional[int]:
        """Get numeric key code from pynput key.

        Args:
            key: Key object from pynput

        Returns:
            Key code if available, None otherwise
        """
        try:
            if hasattr(key, "vk"):
                return key.vk
        except Exception:
            pass
        return None

    @staticmethod
    def _is_modifier_key(key_str: str) -> bool:
        """Check if a key is a modifier key.

        Args:
            key_str: String representation of key

        Returns:
            True if key is a modifier (ctrl, shift, alt, cmd), False otherwise
        """
        modifiers = {
            "ctrl",
            "shift",
            "alt",
            "cmd",
            "ctrl_l",
            "ctrl_r",
            "shift_l",
            "shift_r",
            "alt_l",
            "alt_r",
            "cmd_l",
            "cmd_r",
        }
        return key_str.lower() in modifiers

    @staticmethod
    def _has_significant_movement(
        start: Tuple[int, int], end: Tuple[int, int], threshold: int = 5
    ) -> bool:
        """Check if mouse movement is significant enough to be considered a drag.

        Args:
            start: Starting position (x, y)
            end: Ending position (x, y)
            threshold: Minimum pixel distance to consider as movement

        Returns:
            True if movement exceeds threshold, False otherwise
        """
        distance = ((end[0] - start[0]) ** 2 + (end[1] - start[1]) ** 2) ** 0.5
        return distance > threshold
