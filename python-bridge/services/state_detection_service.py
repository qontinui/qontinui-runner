"""State Detection Service for qontinui-runner.

This service provides a Python bridge for local state detection using the qontinui library.
It processes captured screenshots and input events to build State objects that can be
used for GUI automation.

The service:
1. Loads screenshots from a directory
2. Loads input events from JSON
3. Groups screenshots into transitions
4. Calls qontinui's StateBuilder to construct State objects
5. Serializes the results to JSON for use by qontinui-runner

Usage:
    python3 state_detection_service.py <screenshots_dir> <events.json> <output.json>

Example:
    python3 state_detection_service.py ./captures/session1 ./captures/session1/events.json ./output/states.json
"""

from __future__ import annotations

import json
import sys
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple

import cv2
import numpy as np


# ============================================================================
# Configuration and Data Models
# ============================================================================


@dataclass
class CaptureSession:
    """Represents a complete capture session with screenshots and events."""

    screenshots_dir: Path
    events_file: Path
    output_file: Path


@dataclass
class InputEvent:
    """Represents a user input event (click, keypress, etc.)."""

    timestamp: float
    event_type: str  # 'click', 'key', 'move', etc.
    x: Optional[int] = None
    y: Optional[int] = None
    button: Optional[str] = None
    key: Optional[str] = None
    metadata: Optional[Dict[str, Any]] = None


@dataclass
class TransitionData:
    """Data for a state transition (before/after screenshots + event)."""

    before_screenshot_path: str
    after_screenshot_path: str
    event: InputEvent
    transition_id: int


@dataclass
class DetectedState:
    """Serializable representation of a detected state."""

    name: str
    description: str
    state_images: List[Dict[str, Any]]
    state_regions: List[Dict[str, Any]]
    state_locations: List[Dict[str, Any]]
    boundary: Optional[Dict[str, Any]]
    metadata: Dict[str, Any]


# ============================================================================
# State Detection Service
# ============================================================================


class LocalStateDetectionService:
    """Service for detecting states from local screenshot captures.

    This service bridges qontinui-runner (TypeScript) with the qontinui
    library (Python) to enable local state detection and analysis.

    The service processes a directory of screenshots along with input events
    to construct State objects that represent distinct application states.
    """

    def __init__(self, verbose: bool = True):
        """Initialize the state detection service.

        Args:
            verbose: If True, print progress information to stderr
        """
        self.verbose = verbose
        self._setup_qontinui_imports()

    def _setup_qontinui_imports(self):
        """Setup imports for the qontinui library.

        Adds the qontinui library to the Python path if needed.
        """
        try:
            # Try importing qontinui (should work if installed via pip/poetry)
            from qontinui.discovery.state_construction.state_builder import (
                StateBuilder,
                TransitionInfo,
            )

            self.StateBuilder = StateBuilder
            self.TransitionInfo = TransitionInfo
            self._log("Successfully imported qontinui library")

        except ImportError as e:
            # Try adding parent directory to path
            import os

            qontinui_path = Path(__file__).parent.parent.parent.parent / "qontinui" / "src"
            if qontinui_path.exists():
                sys.path.insert(0, str(qontinui_path))
                self._log(f"Added qontinui to path: {qontinui_path}")

                try:
                    from qontinui.discovery.state_construction.state_builder import (
                        StateBuilder,
                        TransitionInfo,
                    )

                    self.StateBuilder = StateBuilder
                    self.TransitionInfo = TransitionInfo
                    self._log("Successfully imported qontinui library from local path")
                except ImportError as e2:
                    self._log(f"ERROR: Failed to import qontinui library: {e2}")
                    self._log("Please install qontinui: pip install -e /path/to/qontinui")
                    raise
            else:
                self._log(f"ERROR: Failed to import qontinui library: {e}")
                self._log(f"Tried path: {qontinui_path}")
                raise

    def _log(self, message: str):
        """Log a message to stderr if verbose mode is enabled.

        Args:
            message: Message to log
        """
        if self.verbose:
            print(f"[StateDetectionService] {message}", file=sys.stderr, flush=True)

    def process_capture_session(
        self, screenshots_dir: Path, events_file: Path, output_file: Path
    ) -> Dict[str, Any]:
        """Process a capture session to detect states.

        This is the main entry point for the service. It:
        1. Loads screenshots from the directory
        2. Loads input events from JSON
        3. Groups screenshots into transitions
        4. Calls StateBuilder to construct states
        5. Serializes and saves results

        Args:
            screenshots_dir: Directory containing PNG screenshots
            events_file: JSON file with input events
            output_file: Where to write the output JSON

        Returns:
            Dictionary with detected states and metadata

        Raises:
            FileNotFoundError: If screenshots_dir or events_file doesn't exist
            ValueError: If data is invalid or corrupted
        """
        self._log(f"Processing capture session: {screenshots_dir}")

        # Validate inputs
        if not screenshots_dir.exists():
            raise FileNotFoundError(f"Screenshots directory not found: {screenshots_dir}")
        if not events_file.exists():
            raise FileNotFoundError(f"Events file not found: {events_file}")

        # Step 1: Load screenshots
        self._log("Loading screenshots...")
        screenshots, screenshot_files = self._load_screenshots(screenshots_dir)
        self._log(f"Loaded {len(screenshots)} screenshots")

        # Step 2: Load events
        self._log("Loading input events...")
        events = self._load_events(events_file)
        self._log(f"Loaded {len(events)} events")

        # Step 3: Group into transitions
        self._log("Grouping screenshots into transitions...")
        transitions = self._group_into_transitions(screenshots, screenshot_files, events)
        self._log(f"Identified {len(transitions)} transitions")

        # Step 4: Build states using qontinui StateBuilder
        self._log("Building states with qontinui StateBuilder...")
        states = self._build_states(screenshots, transitions)
        self._log(f"Built {len(states)} states")

        # Step 5: Serialize results
        self._log("Serializing states to JSON...")
        result = self._serialize_states(states, transitions, screenshots_dir)

        # Step 6: Write output
        output_file.parent.mkdir(parents=True, exist_ok=True)
        with open(output_file, "w") as f:
            json.dump(result, f, indent=2)
        self._log(f"Wrote results to {output_file}")

        return result

    def _load_screenshots(self, screenshots_dir: Path) -> Tuple[List[np.ndarray], List[Path]]:
        """Load all PNG screenshots from a directory.

        Args:
            screenshots_dir: Directory containing PNG files

        Returns:
            Tuple of (list of screenshot arrays, list of file paths)
        """
        # Find all PNG files, sorted by name (assumed to be chronological)
        screenshot_files = sorted(screenshots_dir.glob("*.png"))

        if not screenshot_files:
            raise ValueError(f"No PNG files found in {screenshots_dir}")

        screenshots = []
        valid_files = []

        for file_path in screenshot_files:
            try:
                # Load with OpenCV (BGR format)
                img = cv2.imread(str(file_path))
                if img is not None:
                    screenshots.append(img)
                    valid_files.append(file_path)
                else:
                    self._log(f"Warning: Failed to load {file_path.name}")
            except Exception as e:
                self._log(f"Warning: Error loading {file_path.name}: {e}")

        return screenshots, valid_files

    def _load_events(self, events_file: Path) -> List[InputEvent]:
        """Load input events from JSON file.

        Expected JSON format:
        {
            "events": [
                {
                    "timestamp": 1234567890.123,
                    "event_type": "click",
                    "x": 100,
                    "y": 200,
                    "button": "left"
                },
                ...
            ]
        }

        Args:
            events_file: Path to JSON file with events

        Returns:
            List of InputEvent objects
        """
        with open(events_file, "r") as f:
            data = json.load(f)

        events = []
        for event_data in data.get("events", []):
            event = InputEvent(
                timestamp=event_data["timestamp"],
                event_type=event_data.get("event_type", "click"),
                x=event_data.get("x"),
                y=event_data.get("y"),
                button=event_data.get("button"),
                key=event_data.get("key"),
                metadata=event_data.get("metadata"),
            )
            events.append(event)

        return events

    def _group_into_transitions(
        self, screenshots: List[np.ndarray], screenshot_files: List[Path], events: List[InputEvent]
    ) -> List:
        """Group screenshots and events into state transitions.

        A transition consists of:
        - Before screenshot (state A)
        - Input event (click, keypress, etc.)
        - After screenshot (state B)

        Args:
            screenshots: List of screenshot arrays
            screenshot_files: List of screenshot file paths
            events: List of input events

        Returns:
            List of TransitionInfo objects for qontinui StateBuilder
        """
        transitions = []

        # Parse timestamps from filenames (assuming format: screenshot_1234567890123.png)
        screenshot_timestamps = []
        for file_path in screenshot_files:
            try:
                # Extract timestamp from filename
                timestamp_str = file_path.stem.split("_")[-1]
                timestamp = float(timestamp_str) / 1000.0  # Convert ms to seconds
                screenshot_timestamps.append(timestamp)
            except (ValueError, IndexError):
                # Fallback: use file modification time
                timestamp = file_path.stat().st_mtime
                screenshot_timestamps.append(timestamp)

        # Match events to screenshots
        for i, event in enumerate(events):
            # Find the screenshot immediately before the event
            before_idx = None
            for j, ts in enumerate(screenshot_timestamps):
                if ts <= event.timestamp:
                    before_idx = j
                else:
                    break

            # Find the screenshot immediately after the event
            after_idx = None
            for j, ts in enumerate(screenshot_timestamps):
                if ts > event.timestamp:
                    after_idx = j
                    break

            # Create transition if we have both before and after
            if before_idx is not None and after_idx is not None:
                click_point = None
                if event.x is not None and event.y is not None:
                    click_point = (event.x, event.y)

                # Create TransitionInfo object
                transition = self.TransitionInfo(
                    before_screenshot=screenshots[before_idx],
                    after_screenshot=screenshots[after_idx],
                    click_point=click_point,
                    input_events=[
                        {
                            "type": event.event_type,
                            "x": event.x,
                            "y": event.y,
                            "button": event.button,
                            "key": event.key,
                        }
                    ],
                    target_state_name=None,  # Will be determined by StateBuilder
                    timestamp=event.timestamp,
                )

                transitions.append(transition)

        return transitions

    def _build_states(
        self, screenshots: List[np.ndarray], transitions: List
    ) -> List[Any]:
        """Build State objects using qontinui StateBuilder.

        This method uses the qontinui library's StateBuilder to analyze
        screenshots and transitions and construct State objects with:
        - StateImages: Persistent visual elements
        - StateRegions: Functional areas
        - StateLocations: Clickable points

        Args:
            screenshots: All captured screenshots
            transitions: List of TransitionInfo objects

        Returns:
            List of State objects from qontinui
        """
        # For now, build a single state from all screenshots
        # In a more advanced implementation, we would:
        # 1. Cluster screenshots by visual similarity
        # 2. Build separate states for each cluster
        # 3. Link states via transitions

        builder = self.StateBuilder(
            consistency_threshold=0.85,
            min_image_area=100,
            min_region_area=500,
        )

        try:
            # Build state from screenshot sequence
            state = builder.build_state_from_screenshots(
                screenshot_sequence=screenshots,
                transitions_from_state=transitions if transitions else None,
            )

            return [state]

        except Exception as e:
            self._log(f"Error building states: {e}")
            # Return empty list on error
            return []

    def _serialize_states(
        self, states: List[Any], transitions: List, screenshots_dir: Path
    ) -> Dict[str, Any]:
        """Serialize State objects to JSON-compatible format.

        Args:
            states: List of State objects from qontinui
            transitions: List of TransitionInfo objects
            screenshots_dir: Directory where screenshots are stored

        Returns:
            Dictionary with serialized states and metadata
        """
        serialized_states = []

        for state in states:
            # Extract StateImages
            state_images = []
            for img in state.state_images:
                state_images.append(
                    {
                        "name": img.name,
                        "similarity": getattr(img, "_similarity", 0.85),
                        "bbox": img.metadata.get("bbox"),
                        "context": img.metadata.get("context"),
                    }
                )

            # Extract StateRegions
            state_regions = []
            for region in state.state_regions:
                state_regions.append(
                    {
                        "name": region.name,
                        "type": region.metadata.get("type"),
                        "bbox": region.metadata.get("bbox"),
                    }
                )

            # Extract StateLocations
            state_locations = []
            for location in state.state_locations:
                state_locations.append(
                    {
                        "name": location.name,
                        "x": location.location.x,
                        "y": location.location.y,
                        "target_state": location.metadata.get("target_state"),
                        "confidence": location.metadata.get("confidence"),
                    }
                )

            # Extract boundary if present
            boundary = None
            if hasattr(state, "usable_area") and state.usable_area is not None:
                boundary = {
                    "x": state.usable_area.x,
                    "y": state.usable_area.y,
                    "width": state.usable_area.w,
                    "height": state.usable_area.h,
                }

            # Create serialized state
            serialized_state = {
                "name": state.name,
                "description": state.description or "",
                "state_images": state_images,
                "state_regions": state_regions,
                "state_locations": state_locations,
                "boundary": boundary,
                "metadata": {
                    "num_images": len(state_images),
                    "num_regions": len(state_regions),
                    "num_locations": len(state_locations),
                },
            }

            serialized_states.append(serialized_state)

        # Create result dictionary
        result = {
            "version": "1.0",
            "service": "LocalStateDetectionService",
            "screenshots_dir": str(screenshots_dir),
            "num_states": len(serialized_states),
            "num_transitions": len(transitions),
            "states": serialized_states,
        }

        return result


# ============================================================================
# CLI Interface
# ============================================================================


def main():
    """CLI entry point for state detection service."""
    if len(sys.argv) != 4:
        print("Usage: python3 state_detection_service.py <screenshots_dir> <events.json> <output.json>")
        print()
        print("Arguments:")
        print("  screenshots_dir  Directory containing PNG screenshots")
        print("  events.json      JSON file with input events")
        print("  output.json      Output file for detected states")
        print()
        print("Example:")
        print("  python3 state_detection_service.py ./captures/session1 ./captures/session1/events.json ./output/states.json")
        sys.exit(1)

    screenshots_dir = Path(sys.argv[1])
    events_file = Path(sys.argv[2])
    output_file = Path(sys.argv[3])

    try:
        # Create service
        service = LocalStateDetectionService(verbose=True)

        # Process session
        result = service.process_capture_session(screenshots_dir, events_file, output_file)

        # Print summary
        print("\n" + "=" * 80)
        print("STATE DETECTION COMPLETE")
        print("=" * 80)
        print(f"Detected States: {result['num_states']}")
        print(f"Transitions: {result['num_transitions']}")
        print(f"Output: {output_file}")
        print("=" * 80)

        sys.exit(0)

    except FileNotFoundError as e:
        print(f"ERROR: {e}", file=sys.stderr)
        sys.exit(1)

    except ValueError as e:
        print(f"ERROR: {e}", file=sys.stderr)
        sys.exit(1)

    except Exception as e:
        print(f"ERROR: Unexpected error: {e}", file=sys.stderr)
        import traceback

        traceback.print_exc()
        sys.exit(1)


if __name__ == "__main__":
    main()
