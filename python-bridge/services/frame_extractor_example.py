"""Frame Extractor Service Example.

This example demonstrates a practical workflow for extracting frames from
a video recording based on user input events, useful for creating training
data or analyzing user interactions.

Example workflow:
1. Load recorded video and input events
2. Filter events (e.g., only button clicks)
3. Extract frames at those events
4. Save frames with metadata for further processing
"""

import json
from pathlib import Path
from typing import List, Dict, Any

from frame_extractor_service import FrameExtractorService, EventFilter
from models.input_event import InputMonitorEvent


def load_events_from_jsonl(jsonl_path: str) -> List[InputMonitorEvent]:
    """Load input events from JSONL file.

    Args:
        jsonl_path: Path to JSONL file containing events

    Returns:
        List of InputMonitorEvent objects
    """
    events = []
    with open(jsonl_path, "r") as f:
        for line in f:
            if line.strip():
                data = json.loads(line)
                events.append(InputMonitorEvent.from_dict(data))
    return events


def create_training_dataset(
    video_path: str,
    events: List[InputMonitorEvent],
    output_dir: Path,
    filter_config: Dict[str, Any],
) -> Dict[str, Any]:
    """Create a training dataset from video and events.

    Extracts frames at filtered events and creates a manifest file
    with frame metadata.

    Args:
        video_path: Path to video recording
        events: List of input events
        output_dir: Directory for output files
        filter_config: Configuration for event filtering

    Returns:
        Dictionary with dataset statistics
    """
    print(f"\n=== Creating Training Dataset ===")
    print(f"Video: {video_path}")
    print(f"Events: {len(events)} total")
    print(f"Output: {output_dir}")

    # Initialize service
    extractor = FrameExtractorService(output_dir, enabled=True)

    # Create event filter from config
    event_filter = EventFilter(
        event_types=filter_config.get("event_types", ["mouse_click"]),
        buttons=filter_config.get("buttons", ["left"]),
        keys=filter_config.get("keys"),
        include_after_delay_ms=filter_config.get("delay_ms", 0),
        max_count=filter_config.get("max_count"),
        time_range=filter_config.get("time_range"),
        modifiers=filter_config.get("modifiers"),
    )

    print(f"\nFilter configuration:")
    print(f"  Event types: {event_filter.event_types}")
    print(f"  Buttons: {event_filter.buttons}")
    print(f"  Keys: {event_filter.keys}")
    print(f"  Delay: {event_filter.include_after_delay_ms}ms")
    print(f"  Max count: {event_filter.max_count}")

    # Extract frames at filtered events
    print(f"\nExtracting frames...")
    frames = extractor.extract_at_events(video_path, events, event_filter)

    if not frames:
        print("No frames extracted - check filter configuration")
        return {"frame_count": 0}

    print(f"Extracted {len(frames)} frames")

    # Save frames
    print(f"\nSaving frames...")
    frames_dir = output_dir / "frames"
    saved_paths = extractor.save_frames_batch(
        frames, str(frames_dir), filename_pattern="frame_{frame_number:05d}", format="png"
    )

    # Create manifest with metadata
    manifest = {
        "video_path": video_path,
        "total_events": len(events),
        "filtered_events": len(frames),
        "filter_config": filter_config,
        "frames": [],
    }

    for frame, path in zip(frames, saved_paths):
        frame_info = {
            "path": path,
            "timestamp": frame.timestamp,
            "frame_number": frame.frame_number,
            "source_event": frame.source_event,
            "metadata": frame.metadata,
        }
        manifest["frames"].append(frame_info)

    # Save manifest
    manifest_path = output_dir / "dataset_manifest.json"
    with open(manifest_path, "w") as f:
        json.dump(manifest, f, indent=2)

    print(f"\nDataset created successfully!")
    print(f"  Frames: {len(frames)}")
    print(f"  Manifest: {manifest_path}")

    return {
        "frame_count": len(frames),
        "manifest_path": str(manifest_path),
        "frames_dir": str(frames_dir),
    }


def analyze_click_patterns(
    video_path: str, events: List[InputMonitorEvent], output_dir: Path
) -> None:
    """Analyze and visualize click patterns from video.

    Extracts frames at each click and organizes them by button type.

    Args:
        video_path: Path to video recording
        events: List of input events
        output_dir: Directory for output files
    """
    print(f"\n=== Analyzing Click Patterns ===")

    extractor = FrameExtractorService(output_dir, enabled=True)

    # Separate by button type
    button_types = ["left", "right", "middle"]

    for button in button_types:
        print(f"\nExtracting {button} clicks...")

        filter = EventFilter(
            event_types=["mouse_click"],
            buttons=[button],
            include_after_delay_ms=50,  # Capture 50ms after click for visual feedback
        )

        frames = extractor.extract_at_events(video_path, events, filter)

        if frames:
            button_dir = output_dir / f"{button}_clicks"
            saved_paths = extractor.save_frames_batch(
                frames,
                str(button_dir),
                filename_pattern=f"{button}_{{frame_number:04d}}",
                format="jpg",
            )
            print(f"  Saved {len(saved_paths)} {button} click frames")

            # Create summary
            summary = {
                "button": button,
                "count": len(frames),
                "timestamps": [f.timestamp for f in frames],
                "positions": [
                    {"x": f.source_event.get("x"), "y": f.source_event.get("y")}
                    for f in frames
                    if f.source_event
                ],
            }

            summary_path = button_dir / "summary.json"
            with open(summary_path, "w") as f:
                json.dump(summary, f, indent=2)


def create_video_thumbnail_strip(
    video_path: str, output_dir: Path, interval_sec: float = 1.0, max_frames: int = 10
) -> None:
    """Create a thumbnail strip from video.

    Extracts frames at regular intervals and saves them.

    Args:
        video_path: Path to video recording
        output_dir: Directory for output files
        interval_sec: Interval between frames in seconds
        max_frames: Maximum number of thumbnails
    """
    print(f"\n=== Creating Video Thumbnail Strip ===")
    print(f"Interval: {interval_sec}s, Max frames: {max_frames}")

    extractor = FrameExtractorService(output_dir, enabled=True)

    # Extract at intervals
    frames = extractor.extract_at_interval(
        video_path, interval_ms=int(interval_sec * 1000), max_count=max_frames
    )

    if frames:
        thumbnails_dir = output_dir / "thumbnails"
        saved_paths = extractor.save_frames_batch(
            frames, str(thumbnails_dir), filename_pattern="thumb_{timestamp:.1f}s", format="jpg"
        )
        print(f"Created {len(saved_paths)} thumbnails in {thumbnails_dir}")


def extract_key_moments(video_path: str, events: List[InputMonitorEvent], output_dir: Path) -> None:
    """Extract frames at key moments based on event patterns.

    Identifies "key moments" such as:
    - First click in a session
    - Keyboard shortcuts (Ctrl+Key)
    - Rapid click sequences

    Args:
        video_path: Path to video recording
        events: List of input events
        output_dir: Directory for output files
    """
    print(f"\n=== Extracting Key Moments ===")

    extractor = FrameExtractorService(output_dir, enabled=True)

    # Key moment 1: Keyboard shortcuts with Ctrl
    print("\n1. Keyboard shortcuts (Ctrl+Key)...")
    filter1 = EventFilter(event_types=["key_press"], modifiers=["ctrl"], include_after_delay_ms=200)
    shortcut_frames = extractor.extract_at_events(video_path, events, filter1)
    if shortcut_frames:
        extractor.save_frames_batch(
            shortcut_frames,
            str(output_dir / "shortcuts"),
            filename_pattern="shortcut_{frame_number:04d}",
            format="png",
        )
        print(f"   Found {len(shortcut_frames)} keyboard shortcuts")

    # Key moment 2: First 5 clicks
    print("\n2. First 5 clicks...")
    filter2 = EventFilter(event_types=["mouse_click"], max_count=5)
    first_clicks = extractor.extract_at_events(video_path, events, filter2)
    if first_clicks:
        extractor.save_frames_batch(
            first_clicks,
            str(output_dir / "first_clicks"),
            filename_pattern="click_{frame_number:04d}",
            format="png",
        )
        print(f"   Saved {len(first_clicks)} initial clicks")

    # Key moment 3: Right clicks (context menu actions)
    print("\n3. Right clicks (context menus)...")
    filter3 = EventFilter(
        event_types=["mouse_click"], buttons=["right"], include_after_delay_ms=100
    )
    context_frames = extractor.extract_at_events(video_path, events, filter3)
    if context_frames:
        extractor.save_frames_batch(
            context_frames,
            str(output_dir / "context_menus"),
            filename_pattern="context_{frame_number:04d}",
            format="png",
        )
        print(f"   Found {len(context_frames)} context menu actions")


def main():
    """Main example execution."""
    print("=" * 60)
    print("  Frame Extractor Service - Practical Examples")
    print("=" * 60)

    # Configuration
    video_path = "path/to/recording.mp4"
    events_path = "path/to/events.jsonl"
    output_base = Path("output/frame_extraction_demo")

    # Example 1: Create training dataset for ML model
    print("\n" + "=" * 60)
    print("Example 1: Create Training Dataset")
    print("=" * 60)

    # Simulate having events
    sample_events = [
        InputMonitorEvent(
            timestamp=1.0, frame_number=30, event_type="mouse_click", x=100, y=200, button="left"
        ),
        InputMonitorEvent(
            timestamp=2.0, frame_number=60, event_type="mouse_click", x=150, y=250, button="left"
        ),
        InputMonitorEvent(
            timestamp=3.0, frame_number=90, event_type="key_press", key="enter", modifiers=["ctrl"]
        ),
    ]

    dataset_config = {
        "event_types": ["mouse_click", "key_press"],
        "buttons": ["left", "right"],
        "delay_ms": 100,
        "max_count": 50,
    }

    # Note: Uncomment to run with actual video file
    # stats = create_training_dataset(
    #     video_path,
    #     sample_events,
    #     output_base / "training_dataset",
    #     dataset_config
    # )

    # Example 2: Analyze click patterns
    print("\n" + "=" * 60)
    print("Example 2: Analyze Click Patterns")
    print("=" * 60)

    # Note: Uncomment to run with actual video file
    # analyze_click_patterns(
    #     video_path,
    #     sample_events,
    #     output_base / "click_analysis"
    # )

    # Example 3: Create thumbnail strip
    print("\n" + "=" * 60)
    print("Example 3: Create Thumbnail Strip")
    print("=" * 60)

    # Note: Uncomment to run with actual video file
    # create_video_thumbnail_strip(
    #     video_path,
    #     output_base / "thumbnails",
    #     interval_sec=2.0,
    #     max_frames=10
    # )

    # Example 4: Extract key moments
    print("\n" + "=" * 60)
    print("Example 4: Extract Key Moments")
    print("=" * 60)

    # Note: Uncomment to run with actual video file
    # extract_key_moments(
    #     video_path,
    #     sample_events,
    #     output_base / "key_moments"
    # )

    print("\n" + "=" * 60)
    print("Examples complete!")
    print("=" * 60)
    print("\nTo run with actual data:")
    print("1. Update video_path and events_path variables")
    print("2. Uncomment the example function calls")
    print("3. Run: python frame_extractor_example.py")


if __name__ == "__main__":
    main()
