# Cloud Streaming Service Integration Guide

## Quick Reference

### File Structure

```
python-bridge/
├── services/
│   ├── cloud_streaming_service.py      # Main service implementation
│   ├── test_cloud_streaming.py          # Test suite and examples
│   ├── cloud_streaming_example.py       # Complete usage example
│   ├── CLOUD_STREAMING_README.md        # Full documentation
│   └── CLOUD_STREAMING_INTEGRATION.md   # This file
└── models/
    ├── input_event.py                   # InputMonitorEvent model
    └── processing_result.py             # ProcessingResult models
```

### Import Statement

```python
from services.cloud_streaming_service import (
    CloudStreamingService,
    StreamConfig,
    StreamedData,
)
```

## Integration Patterns

### 1. Minimal Integration

```python
import asyncio
from services.cloud_streaming_service import CloudStreamingService

async def minimal_streaming():
    service = CloudStreamingService(
        websocket_url="wss://api.example.com/ws/stream"
    )

    await service.connect(jwt_token="your-token")

    # Stream video
    await service.stream_video_chunk(
        session_id="session-123",
        chunk=video_bytes,
        timestamp=time.time()
    )

    await service.disconnect()

asyncio.run(minimal_streaming())
```

### 2. Integration with Video Capture Service

```python
from services.cloud_streaming_service import CloudStreamingService, StreamConfig
from services.video_capture_service import VideoCaptureService

async def integrated_capture_and_stream():
    # Initialize services
    cloud = CloudStreamingService(
        websocket_url="wss://api.example.com/ws/stream",
        config=StreamConfig(
            video_fps=10,
            video_resolution=(1280, 720),
            video_bitrate="1M"
        )
    )

    video = VideoCaptureService(storage_dir=Path("/tmp/captures"))

    # Connect to cloud
    await cloud.connect(jwt_token="token")

    # Start video capture
    session_id = video.start_capture(fps=10)

    # Periodically stream video chunks
    while capturing:
        # Get latest video chunk from video service
        chunk = video.get_latest_chunk()

        # Stream to cloud
        await cloud.stream_video_chunk(
            session_id=session_id,
            chunk=chunk,
            timestamp=time.time()
        )

        await asyncio.sleep(1)  # Stream every second

    await cloud.disconnect()
```

### 3. Integration with Input Monitor Service

```python
from services.cloud_streaming_service import CloudStreamingService
from services.input_monitor_service import InputMonitorService

async def stream_with_events():
    cloud = CloudStreamingService(
        websocket_url="wss://api.example.com/ws/stream"
    )

    input_monitor = InputMonitorService(storage_dir=Path("/tmp/events"))

    # Connect and start monitoring
    await cloud.connect(jwt_token="token")
    input_monitor.start_monitoring(session_id="session-123", fps=30)

    # Periodically stream events
    event_buffer = []

    while monitoring:
        # Collect events
        recent_events = input_monitor.get_recent_events()
        event_buffer.extend(recent_events)

        # Stream when buffer is full
        if len(event_buffer) >= 100:
            await cloud.stream_events(
                session_id="session-123",
                events=event_buffer
            )
            event_buffer.clear()

        await asyncio.sleep(5)  # Check every 5 seconds

    # Stream remaining events
    if event_buffer:
        await cloud.stream_events("session-123", event_buffer)

    await cloud.disconnect()
```

### 4. Complete Session Manager

```python
from services.cloud_streaming_service import CloudStreamingService, StreamConfig
from services.video_capture_service import VideoCaptureService
from services.input_monitor_service import InputMonitorService

class CloudSessionManager:
    """Manages all aspects of cloud streaming for a session."""

    def __init__(self, websocket_url: str, jwt_token: str):
        self.cloud = CloudStreamingService(
            websocket_url=websocket_url,
            config=StreamConfig(
                video_fps=10,
                video_resolution=(1280, 720),
                compress_events=True
            )
        )
        self.jwt_token = jwt_token
        self.session_id = None

        # Background tasks
        self.video_task = None
        self.event_task = None
        self.thumbnail_task = None

    async def start(self, session_id: str):
        """Start streaming session."""
        self.session_id = session_id

        # Connect to cloud
        await self.cloud.connect(self.jwt_token)

        # Start background streaming tasks
        self.video_task = asyncio.create_task(self._stream_video_loop())
        self.event_task = asyncio.create_task(self._stream_events_loop())
        self.thumbnail_task = asyncio.create_task(self._stream_thumbnails_loop())

    async def stop(self):
        """Stop streaming session."""
        # Cancel tasks
        for task in [self.video_task, self.event_task, self.thumbnail_task]:
            if task and not task.done():
                task.cancel()
                try:
                    await task
                except asyncio.CancelledError:
                    pass

        # Disconnect
        await self.cloud.disconnect()

        # Return stats
        return self.cloud.get_stats()

    async def _stream_video_loop(self):
        """Background task for video streaming."""
        while True:
            try:
                # Get video chunk from your capture service
                chunk = get_video_chunk()

                await self.cloud.stream_video_chunk(
                    session_id=self.session_id,
                    chunk=chunk,
                    timestamp=time.time()
                )

                await asyncio.sleep(1)  # 1 chunk per second
            except asyncio.CancelledError:
                break

    async def _stream_events_loop(self):
        """Background task for event streaming."""
        event_buffer = []

        while True:
            try:
                # Collect events from your input monitor
                events = get_recent_events()
                event_buffer.extend(events)

                # Stream when buffer is full
                if len(event_buffer) >= 100:
                    await self.cloud.stream_events(
                        session_id=self.session_id,
                        events=event_buffer
                    )
                    event_buffer.clear()

                await asyncio.sleep(5)  # Check every 5 seconds
            except asyncio.CancelledError:
                # Stream remaining events
                if event_buffer:
                    await self.cloud.stream_events(
                        session_id=self.session_id,
                        events=event_buffer
                    )
                break

    async def _stream_thumbnails_loop(self):
        """Background task for thumbnail streaming."""
        frame_number = 0

        while True:
            try:
                # Capture screenshot
                screenshot = capture_screenshot()

                await self.cloud.stream_thumbnail(
                    session_id=self.session_id,
                    frame_number=frame_number,
                    image=screenshot
                )

                frame_number += 100  # Every 10 seconds at 10 fps
                await asyncio.sleep(10)
            except asyncio.CancelledError:
                break

# Usage
async def main():
    manager = CloudSessionManager(
        websocket_url="wss://api.example.com/ws/stream",
        jwt_token="your-token"
    )

    await manager.start(session_id="session-123")

    # Run for 60 seconds
    await asyncio.sleep(60)

    stats = await manager.stop()
    print(f"Streamed {stats['bytes_sent_mb']:.2f} MB")

asyncio.run(main())
```

## Common Scenarios

### Scenario 1: Stream During Training Data Collection

```python
# In your training data collection script
from services.cloud_streaming_service import CloudStreamingService
from services.unified_data_collector import UnifiedDataCollector

collector = UnifiedDataCollector(...)
cloud = CloudStreamingService(websocket_url="...")

# Start collection
await cloud.connect(jwt_token)
collector.start_collection(session_id)

# ... perform actions ...

# After collection, stream processed data
result = collector.finalize_collection()
await cloud.stream_processed_data(session_id, result)
await cloud.disconnect()
```

### Scenario 2: Real-time Monitoring Dashboard

```python
# Stream data for real-time viewing in web dashboard
async def live_monitoring():
    cloud = CloudStreamingService(
        websocket_url="wss://dashboard.example.com/live",
        config=StreamConfig(
            video_fps=15,  # Higher FPS for smoother viewing
            thumbnail_size=(480, 270),  # Larger thumbnails
            thumbnail_quality=90
        )
    )

    await cloud.connect(jwt_token)

    while monitoring:
        # Stream video and thumbnails frequently
        await cloud.stream_video_chunk(...)
        await cloud.stream_thumbnail(...)

        # Update stats
        stats = cloud.get_stats()
        update_dashboard(stats)

        await asyncio.sleep(0.5)
```

### Scenario 3: Batch Upload Mode

```python
# Collect data locally, then upload in batch
async def batch_upload():
    # Collect data locally first
    local_data = {
        'video_chunks': [],
        'events': [],
        'thumbnails': []
    }

    # ... collect data ...

    # Upload everything to cloud
    cloud = CloudStreamingService(websocket_url="...")
    await cloud.connect(jwt_token)

    # Upload all video chunks
    for i, chunk in enumerate(local_data['video_chunks']):
        await cloud.stream_video_chunk(
            session_id=session_id,
            chunk=chunk['data'],
            timestamp=chunk['timestamp'],
            chunk_index=i
        )

    # Upload all events at once
    await cloud.stream_events(session_id, local_data['events'])

    await cloud.disconnect()
```

## Error Handling

### Reconnection Strategy

The service automatically handles reconnection with exponential backoff:

```python
# Configure reconnection behavior
config = StreamConfig(
    reconnect_delay=2,  # Start with 2 second delay
    max_reconnect_attempts=5  # Try up to 5 times
)

cloud = CloudStreamingService(
    websocket_url="wss://api.example.com/ws/stream",
    config=config,
    on_error=lambda msg: logger.error(f"Cloud error: {msg}")
)
```

### Manual Error Handling

```python
async def resilient_streaming():
    cloud = CloudStreamingService(websocket_url="...")

    max_retries = 3
    retry_count = 0

    while retry_count < max_retries:
        success = await cloud.stream_video_chunk(...)

        if success:
            retry_count = 0  # Reset on success
        else:
            retry_count += 1
            logger.warning(f"Stream failed, retry {retry_count}/{max_retries}")

            if retry_count >= max_retries:
                # Buffer locally for later upload
                save_to_local_buffer(chunk)
            else:
                await asyncio.sleep(2 ** retry_count)  # Exponential backoff
```

## Performance Tuning

### Bandwidth Optimization

```python
# Low bandwidth configuration
low_bandwidth_config = StreamConfig(
    video_fps=5,  # Reduce FPS
    video_resolution=(640, 480),  # Lower resolution
    video_bitrate="500K",  # Lower bitrate
    thumbnail_size=(160, 90),  # Smaller thumbnails
    thumbnail_quality=70,  # Lower quality
    compress_events=True  # Always compress
)
```

### High Quality Configuration

```python
# High quality configuration
hq_config = StreamConfig(
    video_fps=15,
    video_resolution=(1920, 1080),
    video_bitrate="2M",
    thumbnail_size=(480, 270),
    thumbnail_quality=95,
    compress_events=True
)
```

### Batch Size Tuning

```python
# Smaller batches for low latency
event_batch_size = 50
await cloud.stream_events(session_id, events[:50])

# Larger batches for better compression
event_batch_size = 500
await cloud.stream_events(session_id, events[:500])
```

## Monitoring and Debugging

### Enable Debug Logging

```python
import logging

logging.basicConfig(
    level=logging.DEBUG,
    format='%(asctime)s - %(name)s - %(levelname)s - %(message)s'
)

# Now all cloud streaming operations will be logged
```

### Track Statistics

```python
# Monitor stats periodically
async def monitor_stats(cloud: CloudStreamingService):
    while True:
        stats = cloud.get_stats()

        print(f"Status Report:")
        print(f"  Connected: {stats['is_connected']}")
        print(f"  Video chunks: {stats['video_chunks_sent']}")
        print(f"  Events: {stats['events_sent']}")
        print(f"  Data sent: {stats['bytes_sent_mb']:.2f} MB")
        print(f"  Reconnects: {stats['reconnect_attempts']}")

        await asyncio.sleep(30)

# Run monitor in background
asyncio.create_task(monitor_stats(cloud))
```

## Testing

### Unit Testing

```python
import pytest
from services.cloud_streaming_service import StreamConfig, StreamedData

def test_stream_config_defaults():
    config = StreamConfig()
    assert config.video_fps == 10
    assert config.video_resolution == (1280, 720)
    assert config.compress_events == True

def test_streamed_data_serialization():
    data = StreamedData(
        data_type="video",
        session_id="test-123",
        timestamp=1234567890.0,
        data=b"test data",
        metadata={"frame": 42}
    )

    # Test serialization
    data_dict = data.to_dict()
    assert data_dict['data_type'] == "video"
    assert data_dict['session_id'] == "test-123"
    assert 'data' in data_dict  # Base64 encoded

    # Test size calculations
    assert data.get_size_bytes() == len(b"test data")
    assert data.get_size_kb() > 0
```

### Integration Testing

Run the provided test suite:

```bash
cd /path/to/python-bridge/services
python test_cloud_streaming.py
```

## Troubleshooting

### Issue: Connection refused

**Cause**: WebSocket URL incorrect or server not running
**Solution**: Verify URL and server status

### Issue: Authentication failed

**Cause**: Invalid or expired JWT token
**Solution**: Refresh token and reconnect

### Issue: Video chunks rejected

**Cause**: Chunks too large for WebSocket message size
**Solution**: Increase `max_chunk_size` or reduce `video_bitrate`

### Issue: High latency

**Cause**: Too much data being sent
**Solution**: Reduce FPS, resolution, or use more aggressive compression

### Issue: Disconnections during streaming

**Cause**: Network instability
**Solution**: Service will auto-reconnect, consider buffering locally

## Next Steps

1. Review the full documentation: `CLOUD_STREAMING_README.md`
2. Run the test suite: `python test_cloud_streaming.py`
3. Study the complete example: `cloud_streaming_example.py`
4. Integrate with your capture services
5. Deploy to production with proper error handling and monitoring
