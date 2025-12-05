"""Test script for VideoCaptureService.

This script demonstrates basic usage of the VideoCaptureService.
Run this to verify the service is working correctly.

Usage:
    python test_video_capture.py

Note: Requires FFmpeg to be installed and available in PATH.
"""

import sys
import time
from pathlib import Path

# Add parent directory to path for imports
sys.path.insert(0, str(Path(__file__).parent.parent))

from services.video_capture_service import (
    CompressedStreamConfig,
    VideoCaptureConfig,
    VideoCaptureService,
)


def test_basic_recording():
    """Test basic video recording functionality."""
    print("=== Testing VideoCaptureService ===\n")

    # Create service with temp directory
    test_dir = Path(__file__).parent / "test_recordings"
    service = VideoCaptureService(test_dir, enabled=True)

    print(f"Storage directory: {test_dir}")
    print(f"Service enabled: {service.enabled}")
    print(f"Platform: {service._platform}\n")

    # Create configuration
    config = VideoCaptureConfig(
        fps=30, codec="h264", quality="high", resolution="native", audio=False
    )

    stream_config = CompressedStreamConfig(fps=10, resolution=(1280, 720), bitrate="1M")

    print("Video Config:")
    print(f"  FPS: {config.fps}")
    print(
        f"  Quality: {config.quality} (Local CRF: {config.get_crf_value('local')}, Stream CRF: {config.get_crf_value('stream')})"
    )
    print(f"  Codec: {config.codec}")
    print(f"  Resolution: {config.resolution}")
    print(f"  Audio: {config.audio}\n")

    print("Stream Config:")
    print(f"  FPS: {stream_config.fps}")
    print(f"  Resolution: {stream_config.resolution}")
    print(f"  Bitrate: {stream_config.bitrate}\n")

    # Start recording
    session_id = f"test-{int(time.time())}"
    print(f"Starting recording for session: {session_id}")

    success = service.start_recording(session_id, config, stream_config)

    if not success:
        print("ERROR: Failed to start recording")
        print("Make sure FFmpeg is installed and available in PATH")
        return False

    print("Recording started successfully!\n")

    # Check recording status
    print(f"Is recording: {service.is_recording()}")

    # Get session info
    info = service.get_current_session_info()
    if info:
        print("\nCurrent Session Info:")
        print(f"  Session ID: {info['session_id']}")
        print(f"  Start Time: {info['start_time']}")
        print(f"  Local Output: {info['local_output']}")
        print(f"  Stream Output: {info['stream_output']}")

    # Record for 5 seconds
    print("\nRecording for 5 seconds...")
    for i in range(5):
        time.sleep(1)
        frame_count = (
            len(service._current_session.frame_timestamps) if service._current_session else 0
        )
        print(f"  {i+1}s elapsed, {frame_count} frames captured")

    # Stop recording
    print("\nStopping recording...")
    output_path = service.stop_recording()

    if output_path:
        print(f"Recording stopped successfully!")
        print(f"Output file: {output_path}")
        print(f"Is recording: {service.is_recording()}")

        # Check if files exist
        local_file = Path(output_path)
        if local_file.exists():
            size_mb = local_file.stat().st_size / (1024 * 1024)
            print(f"File size: {size_mb:.2f} MB")
        else:
            print("WARNING: Output file does not exist")

        return True
    else:
        print("ERROR: Failed to stop recording")
        return False


def test_frame_timestamps():
    """Test frame timestamp tracking."""
    print("\n=== Testing Frame Timestamp Tracking ===\n")

    test_dir = Path(__file__).parent / "test_recordings"
    service = VideoCaptureService(test_dir, enabled=True)

    config = VideoCaptureConfig(fps=30)
    session_id = f"timestamp-test-{int(time.time())}"

    print(f"Starting recording: {session_id}")
    if not service.start_recording(session_id, config):
        print("ERROR: Failed to start recording")
        return False

    # Record for 3 seconds
    print("Recording for 3 seconds...")
    time.sleep(3)

    # Check some frame timestamps
    if service._current_session:
        frame_count = len(service._current_session.frame_timestamps)
        print(f"Captured {frame_count} frames")

        if frame_count >= 10:
            print("\nSample frame timestamps:")
            for i in [0, 10, 20, 30, frame_count - 1]:
                if i < frame_count:
                    ts = service.get_frame_timestamp(i)
                    if ts:
                        print(f"  Frame {i}: {ts:.3f}")

    service.stop_recording()
    print("Test completed\n")
    return True


def test_disabled_service():
    """Test that disabled service handles operations gracefully."""
    print("\n=== Testing Disabled Service ===\n")

    test_dir = Path(__file__).parent / "test_recordings"
    service = VideoCaptureService(test_dir, enabled=False)

    print(f"Service enabled: {service.enabled}")

    # Try to start recording
    success = service.start_recording("test-session")
    print(f"Start recording result (should be False): {success}")

    # Check recording status
    is_recording = service.is_recording()
    print(f"Is recording (should be False): {is_recording}")

    # Try to get frame timestamp
    ts = service.get_frame_timestamp(0)
    print(f"Frame timestamp (should be None): {ts}")

    print("Disabled service test completed\n")
    return True


if __name__ == "__main__":
    print("VideoCaptureService Test Suite")
    print("=" * 50 + "\n")

    try:
        # Run tests
        test_basic_recording()
        test_frame_timestamps()
        test_disabled_service()

        print("\n" + "=" * 50)
        print("All tests completed!")

    except KeyboardInterrupt:
        print("\nTest interrupted by user")
    except Exception as e:
        print(f"\nTest failed with error: {type(e).__name__}: {e}")
        import traceback

        traceback.print_exc()
