"""Test script for FrameExtractorService.

This script demonstrates the usage of FrameExtractorService with various
extraction methods and event filtering capabilities.

Usage:
    python test_frame_extractor.py [video_path]

If no video_path is provided, the script will create a test video.
"""

import sys
import time
from pathlib import Path
from typing import List

from frame_extractor_service import EventFilter, FrameExtractorService

from models.input_event import InputMonitorEvent


def create_test_events(start_time: float, count: int = 10) -> List[InputMonitorEvent]:
    """Create sample input events for testing.

    Args:
        start_time: Base timestamp for events
        count: Number of events to create

    Returns:
        List of InputMonitorEvent objects
    """
    events = []

    for i in range(count):
        timestamp = start_time + (i * 0.5)  # Events every 500ms

        # Create mix of different event types
        if i % 3 == 0:
            # Mouse click
            event = InputMonitorEvent(
                timestamp=timestamp,
                frame_number=int((timestamp - start_time) * 30),
                event_type="mouse_click",
                x=100 + (i * 10),
                y=200 + (i * 10),
                button="left" if i % 2 == 0 else "right",
            )
        elif i % 3 == 1:
            # Key press
            event = InputMonitorEvent(
                timestamp=timestamp,
                frame_number=int((timestamp - start_time) * 30),
                event_type="key_press",
                key="a" if i % 2 == 0 else "enter",
                modifiers=["ctrl"] if i % 4 == 1 else [],
            )
        else:
            # Mouse move
            event = InputMonitorEvent(
                timestamp=timestamp,
                frame_number=int((timestamp - start_time) * 30),
                event_type="mouse_move",
                x=150 + (i * 15),
                y=250 + (i * 15),
            )

        events.append(event)

    return events


def test_single_frame_extraction(service: FrameExtractorService, video_path: str):
    """Test single frame extraction at a specific timestamp.

    Args:
        service: FrameExtractorService instance
        video_path: Path to test video file
    """
    print("\n=== Test 1: Single Frame Extraction ===")

    # Extract frame at 2.5 seconds
    frame = service.extract_at_timestamp(video_path, 2.5)

    if frame:
        print(f"  Success: Extracted frame at {frame.timestamp}s")
        print(f"  Frame number: {frame.frame_number}")
        print(f"  Image size: {frame.image.size if frame.image else 'N/A'}")

        # Save the frame
        output_path = service.frames_dir / "test_single_frame.png"
        saved_path = service.save_frame(frame, str(output_path))
        print(f"  Saved to: {saved_path}")
    else:
        print("  Failed: Could not extract frame")


def test_event_based_extraction(service: FrameExtractorService, video_path: str):
    """Test frame extraction at input events with filtering.

    Args:
        service: FrameExtractorService instance
        video_path: Path to test video file
    """
    print("\n=== Test 2: Event-Based Extraction ===")

    # Create test events
    start_time = 0.0
    events = create_test_events(start_time, count=15)
    print(f"  Created {len(events)} test events")

    # Test 1: Extract frames at left mouse clicks only
    print("\n  Filter 1: Left clicks only")
    filter1 = EventFilter(event_types=["mouse_click"], buttons=["left"], max_count=5)

    frames = service.extract_at_events(video_path, events, filter1)
    print(f"    Extracted {len(frames)} frames")
    for i, frame in enumerate(frames[:3]):  # Show first 3
        print(
            f"    Frame {i}: timestamp={frame.timestamp:.2f}s, "
            f"event={frame.metadata.get('event_type')}"
        )

    # Test 2: Extract frames at key presses with Ctrl modifier
    print("\n  Filter 2: Ctrl+Key presses")
    filter2 = EventFilter(event_types=["key_press"], modifiers=["ctrl"])

    frames = service.extract_at_events(video_path, events, filter2)
    print(f"    Extracted {len(frames)} frames")

    # Test 3: Extract with delay after events
    print("\n  Filter 3: Left clicks with 100ms delay")
    filter3 = EventFilter(
        event_types=["mouse_click"], buttons=["left"], include_after_delay_ms=100, max_count=3
    )

    frames = service.extract_at_events(video_path, events, filter3)
    print(f"    Extracted {len(frames)} frames")


def test_batch_extraction(service: FrameExtractorService, video_path: str):
    """Test batch extraction at multiple timestamps.

    Args:
        service: FrameExtractorService instance
        video_path: Path to test video file
    """
    print("\n=== Test 3: Batch Extraction ===")

    # Extract frames at specific timestamps
    timestamps = [0.5, 1.0, 1.5, 2.0, 2.5, 3.0]
    print(f"  Extracting {len(timestamps)} frames at: {timestamps}")

    start = time.time()
    frames = service.extract_batch(video_path, timestamps)
    elapsed = time.time() - start

    print(f"  Success: Extracted {len(frames)} frames in {elapsed:.2f}s")
    print(f"  Average: {elapsed/len(frames):.3f}s per frame")

    # Save all frames
    output_dir = service.frames_dir / "batch_test"
    saved_paths = service.save_frames_batch(
        frames, str(output_dir), filename_pattern="batch_frame_{frame_number:04d}", format="png"
    )
    print(f"  Saved {len(saved_paths)} frames to {output_dir}")


def test_interval_extraction(service: FrameExtractorService, video_path: str):
    """Test extraction at regular intervals.

    Args:
        service: FrameExtractorService instance
        video_path: Path to test video file
    """
    print("\n=== Test 4: Interval Extraction ===")

    # Extract frame every 500ms, max 10 frames
    frames = service.extract_at_interval(video_path, interval_ms=500, max_count=10)

    print(f"  Extracted {len(frames)} frames at 500ms intervals")

    if frames:
        print(f"  First frame: {frames[0].timestamp:.2f}s")
        print(f"  Last frame: {frames[-1].timestamp:.2f}s")

        # Save frames
        output_dir = service.frames_dir / "interval_test"
        saved_paths = service.save_frames_batch(
            frames, str(output_dir), filename_pattern="interval_{timestamp:.1f}s", format="jpg"
        )
        print(f"  Saved {len(saved_paths)} frames to {output_dir}")


def test_keyframe_extraction(service: FrameExtractorService, video_path: str):
    """Test keyframe extraction.

    Args:
        service: FrameExtractorService instance
        video_path: Path to test video file
    """
    print("\n=== Test 5: Keyframe Extraction ===")

    frames = service.extract_keyframes(video_path)

    print(f"  Extracted {len(frames)} keyframes")

    if frames:
        print(f"  First keyframe: {frames[0].timestamp:.2f}s")
        print(f"  Last keyframe: {frames[-1].timestamp:.2f}s")

        # Save keyframes
        output_dir = service.frames_dir / "keyframes"
        saved_paths = service.save_frames_batch(
            frames[:5],  # Save first 5 only
            str(output_dir),
            filename_pattern="keyframe_{frame_number:04d}",
            format="png",
        )
        print(f"  Saved {len(saved_paths)} keyframes to {output_dir}")


def create_test_video(output_path: Path) -> bool:
    """Create a simple test video using FFmpeg.

    Args:
        output_path: Path where test video should be created

    Returns:
        True if video was created successfully
    """
    import subprocess

    print("\nCreating test video...")

    try:
        # Create a 5-second test video with color bars
        cmd = [
            "ffmpeg",
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=5:size=640x480:rate=30",
            "-pix_fmt",
            "yuv420p",
            "-y",
            str(output_path),
        ]

        result = subprocess.run(
            cmd,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            creationflags=subprocess.CREATE_NO_WINDOW if sys.platform == "win32" else 0,
        )

        if result.returncode == 0:
            print(f"Test video created: {output_path}")
            return True
        else:
            print(f"Failed to create test video: {result.stderr.decode()}")
            return False

    except FileNotFoundError:
        print("FFmpeg not found. Please provide a video file as argument.")
        return False
    except Exception as e:
        print(f"Error creating test video: {e}")
        return False


def main():
    """Main test function."""
    print("=================================================")
    print("  FrameExtractorService Test Suite")
    print("=================================================")

    # Get video path from command line or create test video
    if len(sys.argv) > 1:
        video_path = sys.argv[1]
        if not Path(video_path).exists():
            print(f"Error: Video file not found: {video_path}")
            return
    else:
        video_path = Path(__file__).parent / "test_video.mp4"
        if not video_path.exists():
            if not create_test_video(video_path):
                print("\nPlease provide a video file as argument:")
                print("  python test_frame_extractor.py <video_path>")
                return

    print(f"\nUsing video: {video_path}")

    # Initialize service
    storage_dir = Path(__file__).parent / "test_output"
    service = FrameExtractorService(storage_dir, enabled=True)

    print(f"Storage directory: {storage_dir}")

    # Run tests
    try:
        test_single_frame_extraction(service, str(video_path))
        test_batch_extraction(service, str(video_path))
        test_interval_extraction(service, str(video_path))
        test_event_based_extraction(service, str(video_path))
        test_keyframe_extraction(service, str(video_path))

        print("\n=================================================")
        print("  All tests completed successfully!")
        print("=================================================")
        print(f"\nCheck output directory: {service.frames_dir}")

    except Exception as e:
        print(f"\nTest failed with error: {e}")
        import traceback

        traceback.print_exc()


if __name__ == "__main__":
    main()
