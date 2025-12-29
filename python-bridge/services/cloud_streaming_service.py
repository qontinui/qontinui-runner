"""Cloud Streaming Service Module.

This module implements the CloudStreamingService class, which handles streaming
compressed video, events, and processed data to AWS/cloud infrastructure.

The service follows the Single Responsibility Principle (SRP) by focusing solely on
cloud data streaming. It handles:
- Streaming compressed video chunks (720p, 10fps, 1Mbps)
- Streaming input events (JSONL, gzipped)
- Generating and streaming thumbnails
- Streaming processed states and transitions
- Handling full-size screenshot requests from cloud

The service is designed to be:
- Cloud-native: Built for AWS/cloud WebSocket streaming
- Efficient: Uses compression for events, optimized video encoding
- Resilient: Automatic reconnection with exponential backoff
- Non-blocking: Async operations for streaming
- Secure: JWT-based authentication

Size Estimates (from architecture):
- Video: ~7.5 MB/min at 720p, 10fps, 1Mbps
- Thumbnails: ~10 KB each
- Events: ~1 KB/100 events (gzipped)
- Processed states: ~50 KB/state
"""

import asyncio
import base64
import gzip
import json
import logging
import time
from collections.abc import Callable
from dataclasses import dataclass, field
from io import BytesIO
from pathlib import Path
from typing import Any

from qontinui_schemas.common import utc_now

try:
    import websockets
    from websockets.client import ClientProtocol as WebSocketClientProtocol
    from websockets.exceptions import ConnectionClosed, WebSocketException
except ImportError:
    websockets = None  # type: ignore
    WebSocketClientProtocol = None  # type: ignore
    ConnectionClosed = None  # type: ignore
    WebSocketException = None  # type: ignore

try:
    from PIL import Image
except ImportError:
    Image = None  # type: ignore[assignment]

# Import models
import contextlib

from models import InputMonitorEvent, ProcessingResult

logger = logging.getLogger(__name__)


@dataclass
class StreamConfig:
    """Configuration for cloud streaming parameters.

    Attributes:
        video_fps: Target frames per second for video streaming (default: 10)
        video_resolution: Target video resolution as (width, height) (default: 1280x720)
        video_bitrate: Target video bitrate for compression (default: "1M" = 1 Mbps)
        thumbnail_size: Thumbnail dimensions as (width, height) (default: 320x180)
        thumbnail_quality: JPEG quality for thumbnails, 0-100 (default: 85)
        compress_events: Whether to gzip compress event data (default: True)
        max_chunk_size: Maximum size in bytes for a single WebSocket message (default: 1MB)
        reconnect_delay: Initial delay in seconds before reconnection attempts (default: 2)
        max_reconnect_attempts: Maximum number of reconnection attempts (default: 5)
        heartbeat_interval: Seconds between heartbeat messages (default: 30)
    """

    video_fps: int = 10
    video_resolution: tuple[int, int] = (1280, 720)
    video_bitrate: str = "1M"
    thumbnail_size: tuple[int, int] = (320, 180)
    thumbnail_quality: int = 85
    compress_events: bool = True
    max_chunk_size: int = 1024 * 1024  # 1 MB
    reconnect_delay: int = 2
    max_reconnect_attempts: int = 5
    heartbeat_interval: int = 30


@dataclass
class StreamedData:
    """Container for data being streamed to cloud.

    Attributes:
        data_type: Type of data being streamed
            - "video": Compressed video chunk
            - "thumbnail": Thumbnail image
            - "events": Input events batch
            - "processed": Processed states/transitions
            - "screenshot": Full-size screenshot
        session_id: Unique identifier for the capture session
        timestamp: Unix timestamp when data was captured
        data: Raw bytes of the data payload (may be compressed)
        metadata: Additional context about the data:
            - frame_number: Frame number for video/thumbnails
            - chunk_index: Sequential chunk number for video
            - event_count: Number of events in batch
            - compression: Compression method used ("gzip", "none")
            - original_size: Size before compression
            - state_count: Number of states in processed data
            - transition_count: Number of transitions in processed data
    """

    data_type: str  # "video", "thumbnail", "events", "processed", "screenshot"
    session_id: str
    timestamp: float
    data: bytes
    metadata: dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization.

        Returns:
            Dictionary with base64-encoded data
        """
        return {
            "data_type": self.data_type,
            "session_id": self.session_id,
            "timestamp": self.timestamp,
            "data": base64.b64encode(self.data).decode("utf-8"),
            "metadata": self.metadata,
        }

    def get_size_bytes(self) -> int:
        """Get the size of the data payload in bytes.

        Returns:
            Size in bytes
        """
        return len(self.data)

    def get_size_kb(self) -> float:
        """Get the size of the data payload in kilobytes.

        Returns:
            Size in KB
        """
        return len(self.data) / 1024.0

    def get_size_mb(self) -> float:
        """Get the size of the data payload in megabytes.

        Returns:
            Size in MB
        """
        return len(self.data) / (1024.0 * 1024.0)


class CloudStreamingService:
    """Service for streaming captured data to cloud infrastructure.

    This service manages WebSocket connections to cloud endpoints and streams
    various types of data including video, events, thumbnails, and processed results.

    Responsibilities:
    1. Establish and maintain WebSocket connection to cloud
    2. Stream compressed video chunks in real-time
    3. Batch and stream input events (with compression)
    4. Generate and stream thumbnail images
    5. Stream processed states and transitions
    6. Handle on-demand screenshot upload requests
    7. Manage reconnection with exponential backoff
    8. Track streaming statistics and health

    The service uses async/await for non-blocking streaming operations and
    implements automatic reconnection on connection failures.

    Usage Example:
        >>> service = CloudStreamingService(
        ...     websocket_url="wss://api.example.com/stream",
        ...     api_url="https://api.example.com"
        ... )
        >>> await service.connect(jwt_token="eyJ...")
        >>>
        >>> # Stream video chunk
        >>> await service.stream_video_chunk(
        ...     session_id="session-123",
        ...     chunk=video_bytes,
        ...     timestamp=time.time()
        ... )
        >>>
        >>> # Stream events
        >>> await service.stream_events(
        ...     session_id="session-123",
        ...     events=[event1, event2, event3]
        ... )
        >>>
        >>> await service.disconnect()
    """

    def __init__(
        self,
        websocket_url: str | None = None,
        api_url: str | None = None,
        config: StreamConfig | None = None,
        on_connected: Callable | None = None,
        on_disconnected: Callable | None = None,
        on_error: Callable | None = None,
    ):
        """Initialize the CloudStreamingService.

        Args:
            websocket_url: WebSocket endpoint URL (e.g., wss://api.example.com/ws/stream)
            api_url: REST API base URL (e.g., https://api.example.com)
            config: StreamConfig with streaming parameters (uses defaults if None)
            on_connected: Callback when connection established
            on_disconnected: Callback when connection lost
            on_error: Callback for errors (receives error message string)

        Raises:
            ImportError: If websockets library is not installed
        """
        if websockets is None:
            raise ImportError(
                "websockets library not installed. Install with: pip install websockets"
            )

        # NOTE: Port 8000 is the main backend, NOT 8001 (qontinui-api)
        self.websocket_url = websocket_url or "wss://localhost:8000/ws/stream"
        self.api_url = api_url or "https://localhost:8000"
        self.config = config or StreamConfig()

        # Callbacks
        self.on_connected = on_connected
        self.on_disconnected = on_disconnected
        self.on_error = on_error

        # Connection state
        self.ws: WebSocketClientProtocol | None = None
        self.is_connected = False
        self.is_running = False
        self.jwt_token: str | None = None
        self.session_id: str | None = None

        # Tasks
        self.heartbeat_task: asyncio.Task | None = None
        self.reconnect_task: asyncio.Task | None = None

        # Statistics
        self.video_chunks_sent = 0
        self.thumbnails_sent = 0
        self.event_batches_sent = 0
        self.events_sent = 0
        self.processed_data_sent = 0
        self.screenshots_sent = 0
        self.bytes_sent = 0
        self.reconnect_attempts = 0
        self.heartbeats_sent = 0

        # Pending screenshot requests
        self.screenshot_requests: dict[str, dict[str, Any]] = {}
        self.screenshot_request_callbacks: dict[str, Callable] = {}

    def _get_timestamp(self) -> str:
        """Get current UTC timestamp in ISO format.

        Returns:
            ISO 8601 formatted timestamp string
        """
        return utc_now().isoformat().replace("+00:00", "Z")

    async def connect(self, jwt_token: str) -> bool:
        """Establish WebSocket connection to cloud.

        Connects to the cloud WebSocket endpoint with JWT authentication.
        Starts the heartbeat task to keep the connection alive.

        Args:
            jwt_token: JWT authentication token for cloud access

        Returns:
            True if connection successful, False otherwise
        """
        try:
            self.jwt_token = jwt_token
            ws_url = f"{self.websocket_url}?token={jwt_token}"

            logger.info(f"Connecting to cloud streaming endpoint: {self.websocket_url}")

            self.ws = await websockets.connect(  # type: ignore[assignment]
                ws_url,
                ping_interval=20,
                ping_timeout=10,
                close_timeout=5,
                max_size=self.config.max_chunk_size * 2,  # Allow larger messages
            )

            self.is_connected = True
            self.is_running = True
            self.reconnect_attempts = 0

            logger.info("Cloud streaming connection established")

            # Start heartbeat task
            if self.heartbeat_task is None or self.heartbeat_task.done():
                self.heartbeat_task = asyncio.create_task(self._heartbeat_loop())

            if self.on_connected:
                try:
                    self.on_connected()
                except Exception as e:
                    logger.error(f"Error in on_connected callback: {e}")

            return True

        except Exception as e:
            logger.error(f"Failed to connect to cloud: {e}")
            self.is_connected = False

            if self.on_error:
                try:
                    self.on_error(f"Connection failed: {e}")
                except Exception as cb_error:
                    logger.error(f"Error in on_error callback: {cb_error}")

            return False

    async def disconnect(self):
        """Close WebSocket connection gracefully.

        Cancels the heartbeat task and closes the WebSocket connection.
        """
        logger.info("Disconnecting from cloud...")
        self.is_running = False

        # Cancel heartbeat task
        if self.heartbeat_task and not self.heartbeat_task.done():
            self.heartbeat_task.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await self.heartbeat_task

        # Close WebSocket
        if self.ws and not self.ws.closed:
            try:
                await self.ws.close()
            except Exception as e:
                logger.error(f"Error closing WebSocket: {e}")

        self.is_connected = False
        self.ws = None

        if self.on_disconnected:
            try:
                self.on_disconnected()
            except Exception as e:
                logger.error(f"Error in on_disconnected callback: {e}")

        logger.info("Disconnected from cloud")

    async def _send_message(
        self,
        message_type: str,
        payload: dict[str, Any],
        wait_for_response: bool = False,
        timeout: float = 10.0,
    ) -> dict[str, Any] | None:
        """Send message to cloud WebSocket server.

        Args:
            message_type: Type of message (e.g., "video_chunk", "events", "heartbeat")
            payload: Message payload dictionary
            wait_for_response: Whether to wait for server response
            timeout: Response timeout in seconds

        Returns:
            Response dictionary if wait_for_response=True and successful, None otherwise
        """
        if not self.is_connected or not self.ws:
            logger.error("Cannot send message: Not connected")
            return None

        try:
            message = {"type": message_type, "timestamp": self._get_timestamp(), **payload}

            # Send message
            await self.ws.send(json.dumps(message))  # type: ignore[attr-defined]

            if wait_for_response:
                # Wait for response
                response_raw = await asyncio.wait_for(self.ws.recv(), timeout=timeout)  # type: ignore[attr-defined]
                response = json.loads(response_raw)

                # Check for error response
                if response.get("type") == "error":
                    error_msg = response.get("error", "Unknown error")
                    details = response.get("details", {})
                    logger.error(f"Cloud error: {error_msg} - {details}")

                    if self.on_error:
                        try:
                            self.on_error(f"{error_msg}: {details}")
                        except Exception as e:
                            logger.error(f"Error in on_error callback: {e}")

                    return None

                return response  # type: ignore[no-any-return]

            return {"success": True}

        except TimeoutError:
            logger.error("Timeout waiting for cloud response")
            return None
        except ConnectionClosed:
            logger.error("Connection closed while sending message")
            self.is_connected = False
            if self.is_running:
                asyncio.create_task(self._reconnect())
            return None
        except Exception as e:
            logger.error(f"Error sending message to cloud: {e}")
            return None

    async def stream_video_chunk(
        self,
        session_id: str,
        chunk: bytes,
        timestamp: float,
        chunk_index: int | None = None,
        frame_number: int | None = None,
    ) -> bool:
        """Stream a compressed video chunk to cloud.

        Video chunks should be pre-compressed using the configured bitrate and resolution.
        Typical size: ~125 KB for 1 second at 1Mbps.

        Args:
            session_id: Unique identifier for the capture session
            chunk: Compressed video data (e.g., H.264/H.265 encoded)
            timestamp: Unix timestamp when chunk was captured
            chunk_index: Sequential index of this chunk (optional)
            frame_number: Starting frame number for this chunk (optional)

        Returns:
            True if chunk sent successfully, False otherwise
        """
        try:
            streamed_data = StreamedData(
                data_type="video",
                session_id=session_id,
                timestamp=timestamp,
                data=chunk,
                metadata={
                    "chunk_index": chunk_index,
                    "frame_number": frame_number,
                    "fps": self.config.video_fps,
                    "resolution": self.config.video_resolution,
                    "bitrate": self.config.video_bitrate,
                    "size_kb": len(chunk) / 1024.0,
                },
            )

            payload = streamed_data.to_dict()
            response = await self._send_message("video_chunk", payload)

            if response:
                self.video_chunks_sent += 1
                self.bytes_sent += len(chunk)
                logger.debug(
                    f"Video chunk sent: {len(chunk)} bytes, "
                    f"chunk_index={chunk_index}, frame={frame_number}"
                )
                return True
            else:
                logger.error("Failed to send video chunk")
                return False

        except Exception as e:
            logger.error(f"Error streaming video chunk: {e}")
            return False

    async def stream_events(self, session_id: str, events: list[InputMonitorEvent]) -> bool:
        """Stream input events to cloud (compressed as JSONL).

        Events are serialized to JSONL format and optionally gzipped for efficient
        transmission. Typical compression: ~1 KB per 100 events.

        Args:
            session_id: Unique identifier for the capture session
            events: List of InputMonitorEvent objects to stream

        Returns:
            True if events sent successfully, False otherwise
        """
        if not events:
            logger.warning("No events to stream")
            return False

        try:
            # Convert events to JSONL format
            jsonl_lines = [event.to_jsonl() for event in events]
            jsonl_data = "\n".join(jsonl_lines)
            jsonl_bytes = jsonl_data.encode("utf-8")

            # Optionally compress with gzip
            if self.config.compress_events:
                compressed_data = gzip.compress(jsonl_bytes, compresslevel=6)
                compression_method = "gzip"
                data_to_send = compressed_data
            else:
                compression_method = "none"
                data_to_send = jsonl_bytes

            streamed_data = StreamedData(
                data_type="events",
                session_id=session_id,
                timestamp=time.time(),
                data=data_to_send,
                metadata={
                    "event_count": len(events),
                    "compression": compression_method,
                    "original_size": len(jsonl_bytes),
                    "compressed_size": len(data_to_send),
                    "compression_ratio": (
                        len(jsonl_bytes) / len(data_to_send) if len(data_to_send) > 0 else 1.0
                    ),
                },
            )

            payload = streamed_data.to_dict()
            response = await self._send_message("events", payload)

            if response:
                self.event_batches_sent += 1
                self.events_sent += len(events)
                self.bytes_sent += len(data_to_send)
                logger.debug(
                    f"Events sent: {len(events)} events, "
                    f"{len(data_to_send)} bytes ({compression_method})"
                )
                return True
            else:
                logger.error("Failed to send events")
                return False

        except Exception as e:
            logger.error(f"Error streaming events: {e}")
            return False

    async def stream_thumbnail(
        self, session_id: str, frame_number: int, image: bytes, timestamp: float | None = None
    ) -> bool:
        """Stream a thumbnail image to cloud.

        Thumbnails are resized and compressed JPEG images for quick preview.
        Typical size: ~10 KB per thumbnail.

        Args:
            session_id: Unique identifier for the capture session
            frame_number: Frame number this thumbnail represents
            image: Raw image bytes (PNG, JPEG, or PIL-compatible format)
            timestamp: Unix timestamp when frame was captured (uses current time if None)

        Returns:
            True if thumbnail sent successfully, False otherwise
        """
        if Image is None:
            logger.error("PIL library not available for thumbnail generation")
            return False

        try:
            # Open image
            if isinstance(image, bytes):
                img = Image.open(BytesIO(image))
            else:
                logger.error("Image must be bytes")
                return False

            # Resize to thumbnail size
            img.thumbnail(self.config.thumbnail_size, Image.LANCZOS)  # type: ignore[attr-defined]

            # Convert to JPEG with compression
            buffer = BytesIO()
            img.convert("RGB").save(
                buffer, format="JPEG", quality=self.config.thumbnail_quality, optimize=True
            )
            thumbnail_bytes = buffer.getvalue()

            streamed_data = StreamedData(
                data_type="thumbnail",
                session_id=session_id,
                timestamp=timestamp or time.time(),
                data=thumbnail_bytes,
                metadata={
                    "frame_number": frame_number,
                    "thumbnail_size": self.config.thumbnail_size,
                    "jpeg_quality": self.config.thumbnail_quality,
                    "size_kb": len(thumbnail_bytes) / 1024.0,
                },
            )

            payload = streamed_data.to_dict()
            response = await self._send_message("thumbnail", payload)

            if response:
                self.thumbnails_sent += 1
                self.bytes_sent += len(thumbnail_bytes)
                logger.debug(
                    f"Thumbnail sent: frame={frame_number}, " f"size={len(thumbnail_bytes)} bytes"
                )
                return True
            else:
                logger.error("Failed to send thumbnail")
                return False

        except Exception as e:
            logger.error(f"Error streaming thumbnail: {e}")
            return False

    async def stream_processed_data(self, session_id: str, data: ProcessingResult) -> bool:
        """Stream processed states and transitions to cloud.

        Sends the complete ProcessingResult including detected states, transitions,
        and processing logs. Typical size: ~50 KB per processing result.

        Args:
            session_id: Unique identifier for the capture session
            data: ProcessingResult containing states, transitions, and logs

        Returns:
            True if data sent successfully, False otherwise
        """
        try:
            # Convert ProcessingResult to JSON
            result_dict = data.to_dict()
            result_json = json.dumps(result_dict, separators=(",", ":"))
            result_bytes = result_json.encode("utf-8")

            # Optionally compress
            if self.config.compress_events:
                compressed_data = gzip.compress(result_bytes, compresslevel=6)
                compression_method = "gzip"
                data_to_send = compressed_data
            else:
                compression_method = "none"
                data_to_send = result_bytes

            streamed_data = StreamedData(
                data_type="processed",
                session_id=session_id,
                timestamp=time.time(),
                data=data_to_send,
                metadata={
                    "state_count": data.num_states,
                    "transition_count": data.num_transitions,
                    "screenshot_count": data.num_screenshots,
                    "processing_success": data.processing_success,
                    "compression": compression_method,
                    "original_size": len(result_bytes),
                    "compressed_size": len(data_to_send),
                    "size_kb": len(data_to_send) / 1024.0,
                },
            )

            payload = streamed_data.to_dict()
            response = await self._send_message("processed_data", payload)

            if response:
                self.processed_data_sent += 1
                self.bytes_sent += len(data_to_send)
                logger.info(
                    f"Processed data sent: {data.num_states} states, "
                    f"{data.num_transitions} transitions, {len(data_to_send)} bytes"
                )
                return True
            else:
                logger.error("Failed to send processed data")
                return False

        except Exception as e:
            logger.error(f"Error streaming processed data: {e}")
            return False

    async def handle_screenshot_request(
        self, request_id: str, filter_params: dict[str, Any]
    ) -> bool:
        """Handle request for full-size screenshots from cloud.

        Stores the request for processing. The actual screenshot upload should be
        triggered by calling upload_screenshots() with matching screenshots.

        Args:
            request_id: Unique identifier for this screenshot request
            filter_params: Filtering criteria:
                - frame_numbers: List of specific frame numbers
                - time_range: Tuple of (start_time, end_time)
                - state_ids: List of state IDs to capture
                - max_count: Maximum number of screenshots

        Returns:
            True if request acknowledged, False otherwise
        """
        try:
            self.screenshot_requests[request_id] = {
                "filter_params": filter_params,
                "timestamp": time.time(),
                "status": "pending",
            }

            logger.info(f"Screenshot request registered: {request_id}")
            return True

        except Exception as e:
            logger.error(f"Error handling screenshot request: {e}")
            return False

    async def upload_screenshots(
        self, session_id: str, screenshots: list[str], request_id: str | None = None
    ) -> bool:
        """Upload full-size screenshots to cloud on demand.

        Uploads high-quality full-resolution screenshots, typically in response
        to a screenshot request from the cloud.

        Args:
            session_id: Unique identifier for the capture session
            screenshots: List of file paths to screenshot images
            request_id: Request ID this upload is responding to (optional)

        Returns:
            True if all screenshots uploaded successfully, False otherwise
        """
        if not screenshots:
            logger.warning("No screenshots to upload")
            return False

        try:
            success_count = 0

            for screenshot_path in screenshots:
                try:
                    # Read screenshot file
                    path = Path(screenshot_path)
                    if not path.exists():
                        logger.warning(f"Screenshot not found: {screenshot_path}")
                        continue

                    screenshot_bytes = path.read_bytes()

                    streamed_data = StreamedData(
                        data_type="screenshot",
                        session_id=session_id,
                        timestamp=time.time(),
                        data=screenshot_bytes,
                        metadata={
                            "request_id": request_id,
                            "filename": path.name,
                            "size_kb": len(screenshot_bytes) / 1024.0,
                        },
                    )

                    payload = streamed_data.to_dict()
                    response = await self._send_message("screenshot", payload)

                    if response:
                        success_count += 1
                        self.screenshots_sent += 1
                        self.bytes_sent += len(screenshot_bytes)
                        logger.debug(f"Screenshot uploaded: {path.name}")
                    else:
                        logger.error(f"Failed to upload screenshot: {path.name}")

                except Exception as e:
                    logger.error(f"Error uploading screenshot {screenshot_path}: {e}")

            # Update request status if applicable
            if request_id and request_id in self.screenshot_requests:
                self.screenshot_requests[request_id]["status"] = "completed"
                self.screenshot_requests[request_id]["uploaded_count"] = success_count

            logger.info(f"Uploaded {success_count}/{len(screenshots)} screenshots")
            return success_count > 0

        except Exception as e:
            logger.error(f"Error uploading screenshots: {e}")
            return False

    async def send_heartbeat(self) -> bool:
        """Send heartbeat to keep connection alive.

        Returns:
            True if heartbeat successful, False otherwise
        """
        if not self.is_connected:
            return False

        try:
            payload = {"stats": self.get_stats()}

            response = await self._send_message(
                "heartbeat", payload, wait_for_response=True, timeout=5.0
            )

            if response and response.get("success"):
                self.heartbeats_sent += 1
                logger.debug("Heartbeat sent successfully")
                return True
            else:
                logger.warning("Heartbeat failed")
                return False

        except Exception as e:
            logger.error(f"Error sending heartbeat: {e}")
            return False

    async def _heartbeat_loop(self):
        """Background task to send heartbeats periodically."""
        logger.info(f"Starting heartbeat loop (interval: {self.config.heartbeat_interval}s)")

        try:
            while self.is_running and self.is_connected:
                await asyncio.sleep(self.config.heartbeat_interval)

                if self.is_connected:
                    success = await self.send_heartbeat()
                    if not success:
                        logger.warning("Heartbeat failed, connection may be lost")

        except asyncio.CancelledError:
            logger.info("Heartbeat loop cancelled")
        except Exception as e:
            logger.error(f"Error in heartbeat loop: {e}")

    async def _reconnect(self):
        """Attempt to reconnect to cloud with exponential backoff."""
        if self.reconnect_task and not self.reconnect_task.done():
            # Already reconnecting
            return

        logger.info("Attempting to reconnect to cloud...")

        while self.reconnect_attempts < self.config.max_reconnect_attempts:
            self.reconnect_attempts += 1
            wait_time = min(
                self.config.reconnect_delay * (2**self.reconnect_attempts), 60
            )  # Exponential backoff, max 60s

            logger.info(
                f"Reconnection attempt {self.reconnect_attempts}/"
                f"{self.config.max_reconnect_attempts} in {wait_time}s..."
            )

            await asyncio.sleep(wait_time)

            if self.jwt_token:
                success = await self.connect(self.jwt_token)
                if success:
                    logger.info("Reconnection successful")
                    return

        logger.error(f"Failed to reconnect after {self.config.max_reconnect_attempts} attempts")

        if self.on_error:
            try:
                self.on_error("Max reconnection attempts reached")
            except Exception as e:
                logger.error(f"Error in on_error callback: {e}")

    def get_stats(self) -> dict[str, Any]:
        """Get streaming statistics.

        Returns:
            Dictionary with streaming statistics including:
            - Connection status
            - Items sent (video chunks, thumbnails, events, etc.)
            - Bytes transferred
            - Reconnection attempts
        """
        return {
            "is_connected": self.is_connected,
            "session_id": self.session_id,
            "video_chunks_sent": self.video_chunks_sent,
            "thumbnails_sent": self.thumbnails_sent,
            "event_batches_sent": self.event_batches_sent,
            "events_sent": self.events_sent,
            "processed_data_sent": self.processed_data_sent,
            "screenshots_sent": self.screenshots_sent,
            "bytes_sent": self.bytes_sent,
            "bytes_sent_mb": self.bytes_sent / (1024.0 * 1024.0),
            "heartbeats_sent": self.heartbeats_sent,
            "reconnect_attempts": self.reconnect_attempts,
            "pending_screenshot_requests": len(self.screenshot_requests),
        }

    def reset_stats(self):
        """Reset all streaming statistics to zero."""
        self.video_chunks_sent = 0
        self.thumbnails_sent = 0
        self.event_batches_sent = 0
        self.events_sent = 0
        self.processed_data_sent = 0
        self.screenshots_sent = 0
        self.bytes_sent = 0
        self.heartbeats_sent = 0
        # Don't reset reconnect_attempts as it's tied to connection health
