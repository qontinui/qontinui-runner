"""Cloud Streaming Service Usage Example.

This example demonstrates a complete integration of CloudStreamingService
with video capture, input monitoring, and processing services for real-time
cloud streaming of a capture session.

Features demonstrated:
- Cloud connection with JWT authentication
- Real-time video chunk streaming
- Periodic thumbnail generation
- Event batching and streaming
- Processed data transmission
- Error handling and reconnection
- Statistics monitoring
"""

import asyncio
import logging
import time
from pathlib import Path
from typing import Any

from cloud_streaming_service import CloudStreamingService, StreamConfig

from models import InputMonitorEvent, ProcessingResult

# Configure logging
logging.basicConfig(
    level=logging.INFO, format="%(asctime)s - %(name)s - %(levelname)s - %(message)s"
)
logger = logging.getLogger(__name__)


class CloudStreamingSession:
    """Manages a complete cloud streaming session.

    This class coordinates all streaming activities for a single capture session,
    including video, events, thumbnails, and processed data.
    """

    def __init__(
        self,
        websocket_url: str,
        api_url: str,
        jwt_token: str,
        session_id: str,
        storage_dir: Path,
        config: StreamConfig | None = None,
    ):
        """Initialize cloud streaming session.

        Args:
            websocket_url: Cloud WebSocket endpoint URL
            api_url: Cloud REST API URL
            jwt_token: JWT authentication token
            session_id: Unique session identifier
            storage_dir: Local storage directory for buffering
            config: Stream configuration (uses defaults if None)
        """
        self.websocket_url = websocket_url
        self.api_url = api_url
        self.jwt_token = jwt_token
        self.session_id = session_id
        self.storage_dir = storage_dir
        self.config = config or StreamConfig()

        # Initialize cloud service
        self.cloud_service = CloudStreamingService(
            websocket_url=websocket_url,
            api_url=api_url,
            config=self.config,
            on_connected=self._on_connected,
            on_disconnected=self._on_disconnected,
            on_error=self._on_error,
        )

        # Session state
        self.is_streaming = False
        self.start_time: float | None = None
        self.chunk_index = 0
        self.event_buffer: list[InputMonitorEvent] = []
        self.errors: list[str] = []

        # Configuration
        self.event_batch_size = 100  # Stream events in batches of 100
        self.thumbnail_interval = 10  # Generate thumbnail every 10 seconds
        self.stats_interval = 30  # Log statistics every 30 seconds

    def _on_connected(self):
        """Callback when cloud connection established."""
        logger.info(f"[{self.session_id}] Connected to cloud successfully")

    def _on_disconnected(self):
        """Callback when cloud connection lost."""
        logger.warning(f"[{self.session_id}] Disconnected from cloud")

    def _on_error(self, error_msg: str):
        """Callback for cloud errors."""
        logger.error(f"[{self.session_id}] Cloud error: {error_msg}")
        self.errors.append(error_msg)

    async def start(self) -> bool:
        """Start the cloud streaming session.

        Returns:
            True if session started successfully, False otherwise
        """
        logger.info(f"[{self.session_id}] Starting cloud streaming session")

        # Connect to cloud
        connected = await self.cloud_service.connect(self.jwt_token)

        if not connected:
            logger.error(f"[{self.session_id}] Failed to connect to cloud")
            return False

        self.is_streaming = True
        self.start_time = time.time()
        self.chunk_index = 0

        logger.info(f"[{self.session_id}] Session started at {self.start_time}")
        return True

    async def stop(self) -> dict[str, Any]:
        """Stop the cloud streaming session.

        Returns:
            Dictionary with session statistics
        """
        logger.info(f"[{self.session_id}] Stopping cloud streaming session")

        self.is_streaming = False

        # Flush any remaining events
        if self.event_buffer:
            await self._stream_events()

        # Get final statistics
        stats = self.cloud_service.get_stats()

        # Add session-specific stats
        session_duration = time.time() - self.start_time if self.start_time else 0
        stats["session_id"] = self.session_id
        stats["session_duration_seconds"] = session_duration
        stats["session_duration_minutes"] = session_duration / 60.0
        stats["errors"] = len(self.errors)
        stats["error_messages"] = self.errors

        # Disconnect from cloud
        await self.cloud_service.disconnect()

        logger.info(f"[{self.session_id}] Session stopped after {session_duration:.1f}s")
        return stats

    async def stream_video_chunk(self, chunk: bytes, frame_number: int) -> bool:
        """Stream a video chunk to cloud.

        Args:
            chunk: Compressed video data
            frame_number: Starting frame number for this chunk

        Returns:
            True if successful, False otherwise
        """
        if not self.is_streaming:
            logger.warning("Cannot stream video: Session not active")
            return False

        try:
            timestamp = time.time()

            success = await self.cloud_service.stream_video_chunk(
                session_id=self.session_id,
                chunk=chunk,
                timestamp=timestamp,
                chunk_index=self.chunk_index,
                frame_number=frame_number,
            )

            if success:
                self.chunk_index += 1
                logger.debug(
                    f"[{self.session_id}] Streamed video chunk {self.chunk_index} "
                    f"({len(chunk)} bytes)"
                )
            else:
                logger.error(f"[{self.session_id}] Failed to stream video chunk")

            return success

        except Exception as e:
            logger.error(f"[{self.session_id}] Error streaming video: {e}")
            return False

    async def add_event(self, event: InputMonitorEvent):
        """Add an input event to the buffer and stream when batch is full.

        Args:
            event: InputMonitorEvent to buffer
        """
        if not self.is_streaming:
            return

        self.event_buffer.append(event)

        # Stream when buffer reaches batch size
        if len(self.event_buffer) >= self.event_batch_size:
            await self._stream_events()

    async def _stream_events(self):
        """Stream buffered events to cloud."""
        if not self.event_buffer:
            return

        try:
            success = await self.cloud_service.stream_events(
                session_id=self.session_id, events=self.event_buffer
            )

            if success:
                logger.debug(f"[{self.session_id}] Streamed {len(self.event_buffer)} events")
                self.event_buffer.clear()
            else:
                logger.error(f"[{self.session_id}] Failed to stream events")

        except Exception as e:
            logger.error(f"[{self.session_id}] Error streaming events: {e}")

    async def stream_thumbnail(self, image: bytes, frame_number: int) -> bool:
        """Stream a thumbnail to cloud.

        Args:
            image: Raw image bytes
            frame_number: Frame number for this thumbnail

        Returns:
            True if successful, False otherwise
        """
        if not self.is_streaming:
            return False

        try:
            success = await self.cloud_service.stream_thumbnail(
                session_id=self.session_id,
                frame_number=frame_number,
                image=image,
                timestamp=time.time(),
            )

            if success:
                logger.debug(f"[{self.session_id}] Streamed thumbnail for frame {frame_number}")
            else:
                logger.error(f"[{self.session_id}] Failed to stream thumbnail")

            return success

        except Exception as e:
            logger.error(f"[{self.session_id}] Error streaming thumbnail: {e}")
            return False

    async def stream_processed_data(self, data: ProcessingResult) -> bool:
        """Stream processed data to cloud.

        Args:
            data: ProcessingResult containing states and transitions

        Returns:
            True if successful, False otherwise
        """
        if not self.is_streaming:
            return False

        try:
            success = await self.cloud_service.stream_processed_data(
                session_id=self.session_id, data=data
            )

            if success:
                logger.info(
                    f"[{self.session_id}] Streamed processed data: "
                    f"{data.num_states} states, {data.num_transitions} transitions"
                )
            else:
                logger.error(f"[{self.session_id}] Failed to stream processed data")

            return success

        except Exception as e:
            logger.error(f"[{self.session_id}] Error streaming processed data: {e}")
            return False

    async def upload_screenshots(
        self, screenshot_paths: list[str], request_id: str | None = None
    ) -> bool:
        """Upload full-size screenshots to cloud.

        Args:
            screenshot_paths: List of file paths to screenshots
            request_id: Optional request ID from cloud

        Returns:
            True if successful, False otherwise
        """
        try:
            success = await self.cloud_service.upload_screenshots(
                session_id=self.session_id, screenshots=screenshot_paths, request_id=request_id
            )

            if success:
                logger.info(f"[{self.session_id}] Uploaded {len(screenshot_paths)} screenshots")
            else:
                logger.error(f"[{self.session_id}] Failed to upload screenshots")

            return success

        except Exception as e:
            logger.error(f"[{self.session_id}] Error uploading screenshots: {e}")
            return False

    def get_stats(self) -> dict[str, Any]:
        """Get current streaming statistics.

        Returns:
            Dictionary with statistics
        """
        stats = self.cloud_service.get_stats()

        # Add session-specific stats
        if self.start_time:
            stats["session_duration_seconds"] = time.time() - self.start_time

        stats["event_buffer_size"] = len(self.event_buffer)
        stats["error_count"] = len(self.errors)

        return stats


async def example_complete_session():
    """Example of a complete cloud streaming session."""
    logger.info("=" * 60)
    logger.info("Cloud Streaming Session Example")
    logger.info("=" * 60)

    # Configuration
    WEBSOCKET_URL = "wss://api.example.com/ws/stream"
    API_URL = "https://api.example.com"
    JWT_TOKEN = "your-jwt-token-here"
    SESSION_ID = f"example-session-{int(time.time())}"
    STORAGE_DIR = Path("/tmp/cloud-streaming-example")

    # Create custom config for high quality
    config = StreamConfig(
        video_fps=10,
        video_resolution=(1280, 720),
        video_bitrate="1M",
        thumbnail_size=(320, 180),
        thumbnail_quality=85,
        compress_events=True,
    )

    # Create session
    session = CloudStreamingSession(
        websocket_url=WEBSOCKET_URL,
        api_url=API_URL,
        jwt_token=JWT_TOKEN,
        session_id=SESSION_ID,
        storage_dir=STORAGE_DIR,
        config=config,
    )

    # Start session
    started = await session.start()

    if not started:
        logger.error("Failed to start session")
        return

    try:
        # Simulate 30 seconds of streaming
        duration = 30
        logger.info(f"Streaming for {duration} seconds...")

        for second in range(duration):
            # Stream video chunk (simulated)
            video_chunk = b"VIDEO_DATA" * 12800  # ~125 KB
            await session.stream_video_chunk(chunk=video_chunk, frame_number=second * 10)

            # Add simulated events
            for i in range(10):
                event = InputMonitorEvent(
                    timestamp=time.time(),
                    frame_number=second * 10 + i,
                    event_type="mouse_move",
                    x=100 + i * 10,
                    y=200 + i * 5,
                )
                await session.add_event(event)

            # Stream thumbnail every 10 seconds
            if second % 10 == 0:
                thumbnail_image = b"THUMBNAIL_DATA" * 1000  # ~10 KB
                await session.stream_thumbnail(image=thumbnail_image, frame_number=second * 10)

            # Log statistics every 10 seconds
            if second % 10 == 0:
                stats = session.get_stats()
                logger.info(
                    f"Progress: {second}s - "
                    f"Video chunks: {stats['video_chunks_sent']}, "
                    f"Events: {stats['events_sent']}, "
                    f"Thumbnails: {stats['thumbnails_sent']}, "
                    f"Data: {stats['bytes_sent_mb']:.2f} MB"
                )

            await asyncio.sleep(1)

        logger.info("Streaming completed successfully")

    except Exception as e:
        logger.error(f"Error during streaming: {e}")

    finally:
        # Stop session and get final stats
        final_stats = await session.stop()

        logger.info("\n" + "=" * 60)
        logger.info("Final Session Statistics")
        logger.info("=" * 60)
        logger.info(f"Session ID: {final_stats['session_id']}")
        logger.info(f"Duration: {final_stats['session_duration_minutes']:.2f} minutes")
        logger.info(f"Video chunks sent: {final_stats['video_chunks_sent']}")
        logger.info(f"Thumbnails sent: {final_stats['thumbnails_sent']}")
        logger.info(f"Event batches sent: {final_stats['event_batches_sent']}")
        logger.info(f"Total events sent: {final_stats['events_sent']}")
        logger.info(f"Total data sent: {final_stats['bytes_sent_mb']:.2f} MB")
        logger.info(f"Errors: {final_stats['errors']}")
        logger.info("=" * 60)


async def example_error_recovery():
    """Example demonstrating error recovery and reconnection."""
    logger.info("\n" + "=" * 60)
    logger.info("Error Recovery Example")
    logger.info("=" * 60)

    session_id = f"recovery-example-{int(time.time())}"

    # Track connection state
    connection_events = []

    def on_connected():
        connection_events.append(("connected", time.time()))
        logger.info("CONNECTED")

    def on_disconnected():
        connection_events.append(("disconnected", time.time()))
        logger.warning("DISCONNECTED")

    def on_error(error_msg):
        connection_events.append(("error", time.time(), error_msg))
        logger.error(f"ERROR: {error_msg}")

    # Create service with callbacks
    service = CloudStreamingService(
        websocket_url="wss://api.example.com/ws/stream",
        on_connected=on_connected,
        on_disconnected=on_disconnected,
        on_error=on_error,
    )

    # Attempt connection (will fail without real server)
    logger.info("Attempting connection...")
    connected = await service.connect(jwt_token="test-token")

    if not connected:
        logger.info("Connection failed (expected without real server)")

    logger.info(f"\nConnection events: {len(connection_events)}")
    for event in connection_events:
        logger.info(f"  {event}")


if __name__ == "__main__":
    # Run examples
    asyncio.run(example_complete_session())
    asyncio.run(example_error_recovery())
