"""Example usage of the InputMonitorService.

This script demonstrates how to use the InputMonitorService to capture
user input events and correlate them with video frames.

Usage:
    python input_monitor_example.py

The script will:
1. Start input monitoring
2. Capture events for 10 seconds
3. Display captured events
4. Save events to a JSONL file
"""

import time
from pathlib import Path

from input_monitor_service import InputMonitorService


def main():
    """Run the input monitoring example."""
    print("=" * 70)
    print("Input Monitor Service - Example Usage")
    print("=" * 70)
    print()

    # Setup storage directory
    storage_dir = Path("/tmp/input_monitor_example")
    storage_dir.mkdir(parents=True, exist_ok=True)
    print(f"Storage directory: {storage_dir}")
    print()

    # Initialize the service
    service = InputMonitorService(storage_dir=storage_dir)
    print("Input monitor service initialized")
    print()

    # Start monitoring
    session_id = f"example_session_{int(time.time())}"
    fps = 30
    print("Starting monitoring...")
    print(f"  Session ID: {session_id}")
    print(f"  FPS: {fps}")
    print()

    success = service.start_monitoring(session_id=session_id, fps=fps)
    if not success:
        print("Failed to start monitoring!")
        return

    print("Monitoring started! Please interact with your computer:")
    print("  - Move your mouse")
    print("  - Click some buttons")
    print("  - Type some keys")
    print("  - Scroll with mouse wheel")
    print("  - Try dragging")
    print()

    # Monitor for 10 seconds
    duration = 10
    for i in range(duration, 0, -1):
        print(f"Capturing for {i} more seconds...", end="\r")
        time.sleep(1)

    print("\n")
    print("Stopping monitoring...")

    # Stop monitoring
    events_file = service.stop_monitoring()
    print(f"Monitoring stopped. Events saved to: {events_file}")
    print()

    # Get all captured events
    events = service.get_events()
    print(f"Total events captured: {len(events)}")
    print()

    # Analyze events by type
    event_counts = {}
    for event in events:
        event_counts[event.event_type] = event_counts.get(event.event_type, 0) + 1

    print("Event breakdown:")
    for event_type, count in sorted(event_counts.items()):
        print(f"  {event_type:20s}: {count:4d} events")
    print()

    # Display first 10 events
    print("First 10 events:")
    print("-" * 70)
    for i, event in enumerate(events[:10]):
        print(
            f"{i+1}. Frame {event.frame_number:4d} ({event.timestamp:.3f}s): "
            f"{event.event_type:15s}",
            end="",
        )

        if event.event_type.startswith("mouse"):
            print(f" at ({event.x:4d}, {event.y:4d})", end="")
            if event.button:
                print(f" [{event.button}]", end="")
            if event.scroll_delta:
                print(f" delta={event.scroll_delta}", end="")
        elif event.event_type.startswith("key"):
            print(f" key='{event.key}'", end="")
            if event.modifiers:
                print(f" modifiers={event.modifiers}", end="")

        if event.duration:
            print(f" ({event.duration:.1f}ms)", end="")

        print()

    print("-" * 70)
    print()

    # Display time range
    if events:
        start_time = events[0].timestamp
        end_time = events[-1].timestamp
        duration = end_time - start_time
        print(f"Time range: {duration:.2f} seconds")
        print(f"Average event rate: {len(events) / duration:.1f} events/second")
        print()

    # Demonstrate reading from JSONL file
    print("Reading events from JSONL file:")
    print("-" * 70)

    with open(events_file) as f:
        lines = f.readlines()
        print(f"File contains {len(lines)} lines")
        print(f"First line: {lines[0][:100]}..." if lines else "File is empty")
        print()

    # Demonstrate time range filtering
    if events and len(events) > 2:
        mid_time = (events[0].timestamp + events[-1].timestamp) / 2
        recent_events = service.get_events_in_range(mid_time, events[-1].timestamp)
        print(f"Events in second half of session: {len(recent_events)}")
        print()

    print("=" * 70)
    print("Example completed successfully!")
    print("=" * 70)


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        print("\n\nExample interrupted by user")
    except Exception as e:
        print(f"\nError: {e}")
        import traceback

        traceback.print_exc()
