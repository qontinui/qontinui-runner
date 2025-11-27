"""Frame Extractor Integration Example.

This example demonstrates how to integrate FrameExtractorService with
VideoCaptureService and InputMonitorService for a complete recording
and analysis workflow.

Workflow:
1. Start video recording and input monitoring
2. User performs actions
3. Stop recording and monitoring
4. Extract frames at interesting input events
5. Process/analyze extracted frames
"""

import time
from pathlib import Path
from typing import List

from video_capture_service import VideoCaptureService, VideoCaptureConfig
from input_monitor_service import InputMonitorService
from frame_extractor_service import FrameExtractorService, EventFilter
from models.input_event import InputMonitorEvent


def complete_recording_workflow_example():
    """Complete workflow: Record, Monitor, Extract, Analyze.

    This demonstrates the full integration of all three services.
    """
    print("=" * 70)
    print("  Complete Recording and Frame Extraction Workflow")
    print("=" * 70)

    # Configuration
    storage_dir = Path("output/workflow_demo")
    session_id = f"session_{int(time.time())}"

    # Step 1: Initialize services
    print("\n[1] Initializing services...")

    video_service = VideoCaptureService(storage_dir, enabled=True)
    input_service = InputMonitorService(storage_dir, enabled=True)
    extractor = FrameExtractorService(storage_dir, enabled=True)

    print("    All services initialized")

    # Step 2: Start recording and monitoring
    print("\n[2] Starting recording and input monitoring...")

    video_config = VideoCaptureConfig(fps=30, quality="high")
    video_started = video_service.start_recording(session_id, video_config)

    if not video_started:
        print("    Failed to start video recording")
        return

    input_started = input_service.start_monitoring(session_id, fps=30)

    if not input_started:
        print("    Failed to start input monitoring")
        video_service.stop_recording()
        return

    print("    Recording and monitoring active")
    print(f"    Session ID: {session_id}")

    # Step 3: Simulate recording duration
    print("\n[3] Recording in progress...")
    print("    (In real usage, user performs actions here)")

    # In real usage, this would be actual user interaction time
    recording_duration = 5  # seconds
    print(f"    Recording for {recording_duration} seconds...")
    time.sleep(recording_duration)

    # Step 4: Stop recording and monitoring
    print("\n[4] Stopping recording and monitoring...")

    events = input_service.stop_monitoring()
    video_path = video_service.stop_recording()

    if not video_path:
        print("    Failed to get video path")
        return

    print(f"    Video saved: {video_path}")
    print(f"    Captured {len(events)} input events")

    # Step 5: Extract frames at interesting events
    print("\n[5] Extracting frames at key events...")

    # Filter: Extract frames at mouse clicks only
    click_filter = EventFilter(
        event_types=["mouse_click"],
        buttons=["left", "right"],
        include_after_delay_ms=50,  # Capture visual feedback
        max_count=20
    )

    click_frames = extractor.extract_at_events(video_path, events, click_filter)
    print(f"    Extracted {len(click_frames)} frames at mouse clicks")

    # Save click frames
    if click_frames:
        click_dir = storage_dir / "click_frames"
        extractor.save_frames_batch(
            click_frames,
            str(click_dir),
            filename_pattern="click_{frame_number:05d}",
            format="png"
        )
        print(f"    Saved to: {click_dir}")

    # Filter: Extract frames at keyboard shortcuts
    shortcut_filter = EventFilter(
        event_types=["key_press"],
        modifiers=["ctrl"],
        include_after_delay_ms=100
    )

    shortcut_frames = extractor.extract_at_events(video_path, events, shortcut_filter)
    print(f"    Extracted {len(shortcut_frames)} frames at keyboard shortcuts")

    if shortcut_frames:
        shortcut_dir = storage_dir / "shortcut_frames"
        extractor.save_frames_batch(
            shortcut_frames,
            str(shortcut_dir),
            filename_pattern="shortcut_{frame_number:05d}",
            format="png"
        )
        print(f"    Saved to: {shortcut_dir}")

    # Step 6: Create overview thumbnails
    print("\n[6] Creating video overview thumbnails...")

    thumbnail_frames = extractor.extract_at_interval(
        video_path,
        interval_ms=1000,  # One frame per second
        max_count=10
    )
    print(f"    Created {len(thumbnail_frames)} thumbnails")

    if thumbnail_frames:
        thumb_dir = storage_dir / "thumbnails"
        extractor.save_frames_batch(
            thumbnail_frames,
            str(thumb_dir),
            filename_pattern="thumb_{timestamp:.1f}s",
            format="jpg"
        )
        print(f"    Saved to: {thumb_dir}")

    # Step 7: Summary
    print("\n[7] Workflow complete!")
    print("    " + "-" * 60)
    print(f"    Session ID: {session_id}")
    print(f"    Video: {video_path}")
    print(f"    Total events: {len(events)}")
    print(f"    Click frames: {len(click_frames)}")
    print(f"    Shortcut frames: {len(shortcut_frames)}")
    print(f"    Thumbnails: {len(thumbnail_frames)}")
    print(f"    Output directory: {storage_dir}")
    print("    " + "-" * 60)


def event_based_analysis_example():
    """Analyze existing recording based on events.

    This example shows how to work with previously recorded data.
    """
    print("\n" + "=" * 70)
    print("  Event-Based Analysis of Existing Recording")
    print("=" * 70)

    # Paths to existing data
    video_path = "recordings/session-123-local.mp4"
    events_file = "recordings/session-123-events.jsonl"
    output_dir = Path("output/analysis")

    # Note: In real usage, you'd load events from file
    # events = load_events_from_jsonl(events_file)

    # For this example, create sample events
    print("\n[1] Loading events...")
    events = create_sample_events()
    print(f"    Loaded {len(events)} events")

    # Initialize extractor
    print("\n[2] Initializing frame extractor...")
    extractor = FrameExtractorService(output_dir, enabled=True)

    # Analyze different event types
    print("\n[3] Analyzing event patterns...")

    analyses = {
        "left_clicks": EventFilter(
            event_types=["mouse_click"],
            buttons=["left"]
        ),
        "right_clicks": EventFilter(
            event_types=["mouse_click"],
            buttons=["right"]
        ),
        "key_presses": EventFilter(
            event_types=["key_press"]
        ),
        "ctrl_shortcuts": EventFilter(
            event_types=["key_press"],
            modifiers=["ctrl"]
        ),
    }

    for name, event_filter in analyses.items():
        print(f"\n    Analyzing: {name}")

        # Note: Uncomment to run with actual video
        # frames = extractor.extract_at_events(video_path, events, event_filter)
        # print(f"      Found {len(frames)} occurrences")

        # if frames:
        #     output_subdir = output_dir / name
        #     extractor.save_frames_batch(
        #         frames,
        #         str(output_subdir),
        #         filename_pattern=f"{name}_{{frame_number:04d}}",
        #         format="png"
        #     )
        #     print(f"      Saved to: {output_subdir}")


def create_sample_events() -> List[InputMonitorEvent]:
    """Create sample events for demonstration."""
    events = []
    base_time = time.time() - 100  # 100 seconds ago

    # Create various event types
    for i in range(20):
        ts = base_time + (i * 2)  # Every 2 seconds

        if i % 3 == 0:
            # Mouse click
            event = InputMonitorEvent(
                timestamp=ts,
                frame_number=i * 60,
                event_type="mouse_click",
                x=100 + i * 20,
                y=200 + i * 15,
                button="left" if i % 2 == 0 else "right"
            )
        elif i % 3 == 1:
            # Key press
            event = InputMonitorEvent(
                timestamp=ts,
                frame_number=i * 60,
                event_type="key_press",
                key=["a", "enter", "space", "s"][i % 4],
                modifiers=["ctrl"] if i % 4 == 1 else []
            )
        else:
            # Mouse move
            event = InputMonitorEvent(
                timestamp=ts,
                frame_number=i * 60,
                event_type="mouse_move",
                x=150 + i * 25,
                y=250 + i * 20
            )

        events.append(event)

    return events


def batch_processing_example():
    """Example of efficient batch processing.

    Shows how to extract and process multiple frame sets efficiently.
    """
    print("\n" + "=" * 70)
    print("  Efficient Batch Processing")
    print("=" * 70)

    video_path = "recordings/session-456-local.mp4"
    output_dir = Path("output/batch_processing")

    print("\n[1] Setting up batch extraction...")
    extractor = FrameExtractorService(output_dir, enabled=True)

    # Define multiple timestamp ranges
    timestamp_sets = {
        "intro": [0.0, 0.5, 1.0, 1.5, 2.0],
        "main_action": [10.0, 10.5, 11.0, 11.5, 12.0],
        "conclusion": [28.0, 28.5, 29.0, 29.5, 30.0]
    }

    print("\n[2] Extracting frames in batches...")

    for name, timestamps in timestamp_sets.items():
        print(f"\n    Processing: {name}")

        # Note: Uncomment to run with actual video
        # frames = extractor.extract_batch(video_path, timestamps)
        # print(f"      Extracted {len(frames)} frames")

        # if frames:
        #     batch_dir = output_dir / name
        #     extractor.save_frames_batch(
        #         frames,
        #         str(batch_dir),
        #         filename_pattern=f"{name}_{{frame_number:04d}}",
        #         format="jpg"
        #     )
        #     print(f"      Saved to: {batch_dir}")


def main():
    """Run all integration examples."""
    print("\n")
    print("*" * 70)
    print("  Frame Extractor Service - Integration Examples")
    print("*" * 70)

    # Example 1: Complete workflow (requires actual recording)
    print("\nExample 1: Complete Recording Workflow")
    print("Note: This example shows the full integration but requires")
    print("      screen recording permissions and actual user input.")
    print("      Uncomment the function call to run with real recording.")

    # Uncomment to run complete workflow:
    # complete_recording_workflow_example()

    # Example 2: Analyze existing recording
    print("\n" + "=" * 70)
    print("Example 2: Event-Based Analysis")
    print("Note: This example requires existing video and event files.")
    print("      Update paths in the function and uncomment to run.")

    event_based_analysis_example()

    # Example 3: Batch processing
    print("\n" + "=" * 70)
    print("Example 3: Efficient Batch Processing")
    print("Note: This example requires existing video files.")
    print("      Update paths in the function and uncomment to run.")

    batch_processing_example()

    print("\n" + "*" * 70)
    print("  Integration examples demonstration complete!")
    print("*" * 70)
    print("\nTo run with actual data:")
    print("1. Ensure FFmpeg is installed and in PATH")
    print("2. Update video_path and events_file paths")
    print("3. Uncomment the example function calls")
    print("4. Run: python frame_extractor_integration_example.py")
    print("\n")


if __name__ == "__main__":
    main()
