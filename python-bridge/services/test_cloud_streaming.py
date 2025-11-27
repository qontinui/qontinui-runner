"""Test suite for CloudStreamingService.

This module provides tests and usage examples for the CloudStreamingService.
It demonstrates how to:
- Connect to cloud streaming endpoints
- Stream video chunks
- Stream input events
- Generate and stream thumbnails
- Stream processed data
- Handle screenshot requests
- Monitor streaming statistics
"""

import asyncio
import gzip
import json
import time
from pathlib import Path
from typing import List

try:
    from PIL import Image
    PIL_AVAILABLE = True
except ImportError:
    PIL_AVAILABLE = False

try:
    # When running as module
    from .cloud_streaming_service import (
        CloudStreamingService,
        StreamConfig,
        StreamedData,
    )
    from ..models import (
        InputMonitorEvent,
        ProcessingResult,
        ProcessingLog,
        ProcessingStep,
        DetectedState,
        Transition,
    )
except ImportError:
    # When running directly
    from cloud_streaming_service import (
        CloudStreamingService,
        StreamConfig,
        StreamedData,
    )
    import sys
    from pathlib import Path
    sys.path.append(str(Path(__file__).parent.parent))
    from models import (
        InputMonitorEvent,
        ProcessingResult,
        ProcessingLog,
        ProcessingStep,
        DetectedState,
        Transition,
    )


def create_test_video_chunk(size_kb: int = 125) -> bytes:
    """Create a dummy video chunk for testing.

    Args:
        size_kb: Size of the chunk in KB (default: 125 KB = ~1 second at 1Mbps)

    Returns:
        Random bytes representing a video chunk
    """
    return b"VIDEO_CHUNK_DATA" * (size_kb * 1024 // 16)


def create_test_image(width: int = 1920, height: int = 1080) -> bytes:
    """Create a test image for thumbnail generation.

    Args:
        width: Image width in pixels
        height: Image height in pixels

    Returns:
        PNG image bytes
    """
    if not PIL_AVAILABLE:
        # Return a dummy PNG header for testing
        return b'\x89PNG\r\n\x1a\n' + b'\x00' * 1024

    # Create a simple gradient image
    img = Image.new('RGB', (width, height))
    pixels = img.load()

    for y in range(height):
        for x in range(width):
            pixels[x, y] = (
                int(255 * x / width),
                int(255 * y / height),
                128
            )

    from io import BytesIO
    buffer = BytesIO()
    img.save(buffer, format='PNG')
    return buffer.getvalue()


def create_test_events(count: int = 100) -> List[InputMonitorEvent]:
    """Create test input events.

    Args:
        count: Number of events to create

    Returns:
        List of InputMonitorEvent objects
    """
    events = []
    start_time = time.time() - 10.0  # 10 seconds ago

    for i in range(count):
        timestamp = start_time + (i * 0.1)  # Events every 100ms
        frame_number = int((timestamp - start_time) * 30)  # 30 fps

        if i % 4 == 0:
            # Mouse click event
            event = InputMonitorEvent(
                timestamp=timestamp,
                frame_number=frame_number,
                event_type="mouse_click",
                x=100 + (i * 10) % 500,
                y=100 + (i * 5) % 300,
                button="left",
                duration=0.05,
            )
        elif i % 4 == 1:
            # Mouse move event
            event = InputMonitorEvent(
                timestamp=timestamp,
                frame_number=frame_number,
                event_type="mouse_move",
                x=200 + (i * 15) % 600,
                y=150 + (i * 8) % 400,
            )
        elif i % 4 == 2:
            # Key press event
            event = InputMonitorEvent(
                timestamp=timestamp,
                frame_number=frame_number,
                event_type="key_press",
                key=chr(97 + (i % 26)),  # a-z
                modifiers=[],
            )
        else:
            # Mouse scroll event
            event = InputMonitorEvent(
                timestamp=timestamp,
                frame_number=frame_number,
                event_type="mouse_scroll",
                x=300,
                y=250,
                scroll_delta=120 if i % 2 == 0 else -120,
            )

        events.append(event)

    return events


def create_test_processing_result(session_id: str) -> ProcessingResult:
    """Create a test ProcessingResult.

    Args:
        session_id: Session ID for the result

    Returns:
        ProcessingResult with sample states and transitions
    """
    # Create processing steps
    steps = [
        ProcessingStep(
            name="frame_extraction",
            start_time=time.time() - 100,
            end_time=time.time() - 90,
            input_count=300,
            output_count=300,
            parameters={"fps": 10},
            success=True,
        ),
        ProcessingStep(
            name="clustering",
            start_time=time.time() - 90,
            end_time=time.time() - 80,
            input_count=300,
            output_count=5,
            parameters={"method": "kmeans", "n_clusters": 5},
            success=True,
        ),
        ProcessingStep(
            name="state_detection",
            start_time=time.time() - 80,
            end_time=time.time() - 70,
            input_count=5,
            output_count=5,
            parameters={"confidence_threshold": 0.8},
            success=True,
        ),
    ]

    # Create processing log
    processing_log = ProcessingLog(
        steps=steps,
        parameters_used={"fps": 10, "resolution": (1280, 720)},
        confidence_scores={"avg_state_confidence": 0.92},
        start_time=time.time() - 100,
        end_time=time.time() - 70,
    )

    # Create sample states
    states = [
        DetectedState(
            id="state-001",
            name="Login Screen",
            confidence=0.95,
            frame_numbers=[0, 1, 2, 3, 4],
            representative_frame=2,
            timestamp=time.time() - 100,
        ),
        DetectedState(
            id="state-002",
            name="Dashboard",
            confidence=0.92,
            frame_numbers=[5, 6, 7, 8],
            representative_frame=6,
            timestamp=time.time() - 95,
        ),
        DetectedState(
            id="state-003",
            name="Settings Page",
            confidence=0.88,
            frame_numbers=[9, 10, 11],
            representative_frame=10,
            timestamp=time.time() - 90,
        ),
    ]

    # Create sample transitions
    transitions = [
        Transition(
            id="transition-001",
            source_states=["state-001"],
            target_states=["state-002"],
            trigger_events=["mouse_click"],
            confidence=0.90,
        ),
        Transition(
            id="transition-002",
            source_states=["state-002"],
            target_states=["state-003"],
            trigger_events=["mouse_click"],
            confidence=0.85,
        ),
    ]

    return ProcessingResult(
        session_id=session_id,
        states=states,
        transitions=transitions,
        processing_log=processing_log,
        source_screenshots=["screenshot-0001.png", "screenshot-0002.png"],
    )


async def test_connection():
    """Test basic connection and disconnection."""
    print("\n=== Testing Connection ===")

    service = CloudStreamingService(
        websocket_url="wss://localhost:8001/ws/stream",
        api_url="https://localhost:8001"
    )

    # Test connection (will fail without real server, but demonstrates usage)
    fake_token = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.test"

    print("Attempting to connect to cloud...")
    connected = await service.connect(fake_token)

    if connected:
        print("Connected successfully!")
        print(f"Connection stats: {service.get_stats()}")

        # Test disconnect
        await service.disconnect()
        print("Disconnected successfully!")
    else:
        print("Connection failed (expected without real server)")

    return service


async def test_video_streaming():
    """Test video chunk streaming."""
    print("\n=== Testing Video Streaming ===")

    service = CloudStreamingService()
    session_id = "test-session-001"

    # Simulate streaming 10 video chunks
    for i in range(10):
        chunk = create_test_video_chunk(size_kb=125)  # ~1 second at 1Mbps
        timestamp = time.time()

        print(f"Streaming chunk {i + 1}/10 ({len(chunk)} bytes)...")

        # This would actually stream if connected
        # await service.stream_video_chunk(
        #     session_id=session_id,
        #     chunk=chunk,
        #     timestamp=timestamp,
        #     chunk_index=i,
        #     frame_number=i * 10
        # )

        # Instead, demonstrate the data structure
        streamed_data = StreamedData(
            data_type="video",
            session_id=session_id,
            timestamp=timestamp,
            data=chunk,
            metadata={
                "chunk_index": i,
                "frame_number": i * 10,
                "fps": 10,
                "size_kb": len(chunk) / 1024.0,
            }
        )

        print(f"  Size: {streamed_data.get_size_kb():.2f} KB")
        print(f"  Metadata: {streamed_data.metadata}")

    print(f"\nTotal video data: {10 * 125} KB = {10 * 125 / 1024:.2f} MB")


async def test_event_streaming():
    """Test event streaming with compression."""
    print("\n=== Testing Event Streaming ===")

    service = CloudStreamingService()
    session_id = "test-session-001"

    # Create test events
    events = create_test_events(count=100)
    print(f"Created {len(events)} test events")

    # Convert to JSONL and compress
    jsonl_lines = [event.to_jsonl() for event in events]
    jsonl_data = "\n".join(jsonl_lines)
    jsonl_bytes = jsonl_data.encode('utf-8')
    compressed_data = gzip.compress(jsonl_bytes, compresslevel=6)

    print(f"Original size: {len(jsonl_bytes)} bytes ({len(jsonl_bytes) / 1024:.2f} KB)")
    print(f"Compressed size: {len(compressed_data)} bytes ({len(compressed_data) / 1024:.2f} KB)")
    print(f"Compression ratio: {len(jsonl_bytes) / len(compressed_data):.2f}x")

    # This would actually stream if connected
    # await service.stream_events(session_id=session_id, events=events)


async def test_thumbnail_streaming():
    """Test thumbnail generation and streaming."""
    print("\n=== Testing Thumbnail Streaming ===")

    if not PIL_AVAILABLE:
        print("PIL not available, skipping thumbnail test")
        return

    service = CloudStreamingService(
        config=StreamConfig(
            thumbnail_size=(320, 180),
            thumbnail_quality=85
        )
    )
    session_id = "test-session-001"

    # Create test image
    full_size_image = create_test_image(width=1920, height=1080)
    print(f"Full-size image: {len(full_size_image)} bytes ({len(full_size_image) / 1024:.2f} KB)")

    # Generate thumbnail (simulated)
    img = Image.open(BytesIO(full_size_image))
    img.thumbnail((320, 180), Image.LANCZOS)

    from io import BytesIO
    buffer = BytesIO()
    img.convert('RGB').save(buffer, format='JPEG', quality=85, optimize=True)
    thumbnail_bytes = buffer.getvalue()

    print(f"Thumbnail size: {len(thumbnail_bytes)} bytes ({len(thumbnail_bytes) / 1024:.2f} KB)")
    print(f"Size reduction: {len(full_size_image) / len(thumbnail_bytes):.2f}x")

    # This would actually stream if connected
    # await service.stream_thumbnail(
    #     session_id=session_id,
    #     frame_number=42,
    #     image=full_size_image,
    #     timestamp=time.time()
    # )


async def test_processed_data_streaming():
    """Test streaming of processed states and transitions."""
    print("\n=== Testing Processed Data Streaming ===")

    service = CloudStreamingService()
    session_id = "test-session-001"

    # Create test processing result
    result = create_test_processing_result(session_id)

    print(f"Processing Result:")
    print(f"  States: {result.num_states}")
    print(f"  Transitions: {result.num_transitions}")
    print(f"  Screenshots: {result.num_screenshots}")
    print(f"  Success: {result.processing_success}")

    # Convert to JSON and compress
    result_dict = result.to_dict()
    result_json = json.dumps(result_dict, separators=(',', ':'))
    result_bytes = result_json.encode('utf-8')
    compressed_data = gzip.compress(result_bytes, compresslevel=6)

    print(f"\nData sizes:")
    print(f"  Original: {len(result_bytes)} bytes ({len(result_bytes) / 1024:.2f} KB)")
    print(f"  Compressed: {len(compressed_data)} bytes ({len(compressed_data) / 1024:.2f} KB)")
    print(f"  Compression ratio: {len(result_bytes) / len(compressed_data):.2f}x")

    # This would actually stream if connected
    # await service.stream_processed_data(session_id=session_id, data=result)


async def test_screenshot_upload():
    """Test screenshot upload handling."""
    print("\n=== Testing Screenshot Upload ===")

    service = CloudStreamingService()
    session_id = "test-session-001"

    # Register screenshot request
    request_id = "request-001"
    filter_params = {
        "frame_numbers": [10, 20, 30],
        "max_count": 5
    }

    await service.handle_screenshot_request(request_id, filter_params)
    print(f"Screenshot request registered: {request_id}")
    print(f"Filter params: {filter_params}")

    # Simulate screenshot paths
    screenshot_paths = [
        "/tmp/screenshots/screenshot-0010.png",
        "/tmp/screenshots/screenshot-0020.png",
        "/tmp/screenshots/screenshot-0030.png",
    ]

    print(f"\nWould upload {len(screenshot_paths)} screenshots")
    print(f"Pending requests: {len(service.screenshot_requests)}")

    # This would actually upload if connected and files existed
    # await service.upload_screenshots(
    #     session_id=session_id,
    #     screenshots=screenshot_paths,
    #     request_id=request_id
    # )


async def test_statistics():
    """Test statistics tracking."""
    print("\n=== Testing Statistics ===")

    service = CloudStreamingService()

    # Simulate some activity
    service.video_chunks_sent = 120  # 2 minutes at 1 chunk/second
    service.thumbnails_sent = 12  # 1 thumbnail every 10 seconds
    service.event_batches_sent = 24  # 1 batch every 5 seconds
    service.events_sent = 2400  # 100 events per batch
    service.processed_data_sent = 1
    service.screenshots_sent = 5
    service.bytes_sent = 120 * 125 * 1024  # 120 chunks * 125 KB each
    service.heartbeats_sent = 4  # Every 30 seconds

    stats = service.get_stats()

    print("Streaming Statistics:")
    print(f"  Video chunks sent: {stats['video_chunks_sent']}")
    print(f"  Thumbnails sent: {stats['thumbnails_sent']}")
    print(f"  Event batches sent: {stats['event_batches_sent']}")
    print(f"  Total events sent: {stats['events_sent']}")
    print(f"  Processed data sent: {stats['processed_data_sent']}")
    print(f"  Screenshots sent: {stats['screenshots_sent']}")
    print(f"  Total bytes sent: {stats['bytes_sent']:,} ({stats['bytes_sent_mb']:.2f} MB)")
    print(f"  Heartbeats sent: {stats['heartbeats_sent']}")


async def test_config_customization():
    """Test custom configuration."""
    print("\n=== Testing Configuration ===")

    # Default config
    default_config = StreamConfig()
    print("Default Configuration:")
    print(f"  Video FPS: {default_config.video_fps}")
    print(f"  Video Resolution: {default_config.video_resolution}")
    print(f"  Video Bitrate: {default_config.video_bitrate}")
    print(f"  Thumbnail Size: {default_config.thumbnail_size}")
    print(f"  Thumbnail Quality: {default_config.thumbnail_quality}")
    print(f"  Compress Events: {default_config.compress_events}")
    print(f"  Max Chunk Size: {default_config.max_chunk_size / 1024:.0f} KB")

    # Custom config for higher quality
    hq_config = StreamConfig(
        video_fps=15,
        video_resolution=(1920, 1080),
        video_bitrate="2M",
        thumbnail_size=(480, 270),
        thumbnail_quality=95,
        compress_events=True,
        max_chunk_size=2 * 1024 * 1024,  # 2 MB
    )

    print("\nHigh-Quality Configuration:")
    print(f"  Video FPS: {hq_config.video_fps}")
    print(f"  Video Resolution: {hq_config.video_resolution}")
    print(f"  Video Bitrate: {hq_config.video_bitrate}")
    print(f"  Thumbnail Size: {hq_config.thumbnail_size}")
    print(f"  Thumbnail Quality: {hq_config.thumbnail_quality}")

    service = CloudStreamingService(config=hq_config)
    print(f"\nService created with custom config")


async def main():
    """Run all tests."""
    print("=" * 60)
    print("CloudStreamingService Test Suite")
    print("=" * 60)

    await test_connection()
    await test_video_streaming()
    await test_event_streaming()
    await test_thumbnail_streaming()
    await test_processed_data_streaming()
    await test_screenshot_upload()
    await test_statistics()
    await test_config_customization()

    print("\n" + "=" * 60)
    print("All tests completed!")
    print("=" * 60)


if __name__ == "__main__":
    asyncio.run(main())
