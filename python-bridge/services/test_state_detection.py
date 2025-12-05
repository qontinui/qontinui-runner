"""Test script for state_detection_service.py

This script creates sample data and tests the state detection service.
It's useful for verifying the service works correctly without needing
real capture data.
"""

from __future__ import annotations

import json
import tempfile
from pathlib import Path

import cv2
import numpy as np
from state_detection_service import LocalStateDetectionService


def create_sample_screenshot(width=800, height=600, text="State", color=(255, 255, 255)):
    """Create a sample screenshot with some visual elements.

    Args:
        width: Image width
        height: Image height
        text: Text to draw on the image
        color: Background color (BGR)

    Returns:
        numpy array with the screenshot
    """
    # Create blank image
    img = np.zeros((height, width, 3), dtype=np.uint8)
    img[:] = color

    # Add a title bar
    cv2.rectangle(img, (0, 0), (width, 50), (100, 100, 100), -1)
    cv2.putText(
        img, "Application Title", (10, 30), cv2.FONT_HERSHEY_SIMPLEX, 0.8, (255, 255, 255), 2
    )

    # Add main text
    cv2.putText(
        img, text, (width // 2 - 100, height // 2), cv2.FONT_HERSHEY_SIMPLEX, 1.5, (0, 0, 0), 3
    )

    # Add some buttons
    button_y = height - 100
    cv2.rectangle(img, (50, button_y), (200, button_y + 40), (200, 200, 200), -1)
    cv2.putText(img, "Button 1", (70, button_y + 27), cv2.FONT_HERSHEY_SIMPLEX, 0.7, (0, 0, 0), 2)

    cv2.rectangle(img, (250, button_y), (400, button_y + 40), (200, 200, 200), -1)
    cv2.putText(img, "Button 2", (270, button_y + 27), cv2.FONT_HERSHEY_SIMPLEX, 0.7, (0, 0, 0), 2)

    return img


def create_sample_data(temp_dir: Path):
    """Create sample screenshots and events for testing.

    Args:
        temp_dir: Temporary directory to store test data

    Returns:
        Tuple of (screenshots_dir, events_file)
    """
    screenshots_dir = temp_dir / "screenshots"
    screenshots_dir.mkdir(exist_ok=True)

    # Create multiple screenshots (simulating a capture session)
    base_timestamp = 1700000000000  # Nov 2023

    # State 1: Main menu (5 screenshots)
    for i in range(5):
        img = create_sample_screenshot(text="Main Menu", color=(240, 240, 240))
        timestamp = base_timestamp + i * 100
        filename = f"screenshot_{timestamp}.png"
        cv2.imwrite(str(screenshots_dir / filename), img)

    # Transition screenshot (between states)
    img = create_sample_screenshot(text="Loading...", color=(200, 200, 200))
    timestamp = base_timestamp + 500
    cv2.imwrite(str(screenshots_dir / f"screenshot_{timestamp}.png"), img)

    # State 2: Settings (5 screenshots)
    for i in range(5):
        img = create_sample_screenshot(text="Settings", color=(230, 230, 250))
        timestamp = base_timestamp + 600 + i * 100
        filename = f"screenshot_{timestamp}.png"
        cv2.imwrite(str(screenshots_dir / filename), img)

    # Create events file
    events = {
        "events": [
            {
                "timestamp": (base_timestamp + 250) / 1000.0,  # Convert ms to seconds
                "event_type": "click",
                "x": 125,
                "y": 550,
                "button": "left",
            },
            {
                "timestamp": (base_timestamp + 850) / 1000.0,
                "event_type": "click",
                "x": 325,
                "y": 550,
                "button": "left",
            },
        ]
    }

    events_file = temp_dir / "events.json"
    with open(events_file, "w") as f:
        json.dump(events, f, indent=2)

    return screenshots_dir, events_file


def test_state_detection():
    """Test the state detection service with sample data."""
    print("=" * 80)
    print("STATE DETECTION SERVICE TEST")
    print("=" * 80)
    print()

    # Create temporary directory for test data
    with tempfile.TemporaryDirectory() as temp_dir:
        temp_path = Path(temp_dir)

        print("1. Creating sample data...")
        screenshots_dir, events_file = create_sample_data(temp_path)
        print(f"   Screenshots: {screenshots_dir}")
        print(f"   Events: {events_file}")
        print()

        # Create output file
        output_file = temp_path / "states.json"

        print("2. Running state detection service...")
        print()

        try:
            # Create service
            service = LocalStateDetectionService(verbose=True)

            # Process capture session
            result = service.process_capture_session(screenshots_dir, events_file, output_file)

            print()
            print("3. Results:")
            print(f"   Detected states: {result['num_states']}")
            print(f"   Transitions: {result['num_transitions']}")
            print()

            # Print details of each state
            for i, state in enumerate(result["states"], 1):
                print(f"   State {i}: {state['name']}")
                print(f"      Description: {state['description']}")
                print(f"      StateImages: {len(state['state_images'])}")
                print(f"      StateRegions: {len(state['state_regions'])}")
                print(f"      StateLocations: {len(state['state_locations'])}")
                print()

            print("=" * 80)
            print("TEST PASSED")
            print("=" * 80)

            return True

        except Exception as e:
            print()
            print("=" * 80)
            print("TEST FAILED")
            print("=" * 80)
            print(f"Error: {e}")
            import traceback

            traceback.print_exc()
            return False


def main():
    """Main entry point for test script."""
    import sys

    success = test_state_detection()
    sys.exit(0 if success else 1)


if __name__ == "__main__":
    main()
