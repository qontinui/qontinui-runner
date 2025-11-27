"""
WebSocket client for qontinui-runner to communicate with qontinui-web backend.

This module provides real-time bidirectional communication for:
- Live screenshot streaming
- Automation log streaming
- Session tracking with heartbeats
- Connection management with auto-reconnection
"""

import asyncio
import json
import base64
import logging
from datetime import datetime, timezone
from typing import Optional, Dict, Any, Callable
from io import BytesIO
import platform
import socket

try:
    import websockets
    from websockets.client import WebSocketClientProtocol
    from websockets.exceptions import ConnectionClosed, WebSocketException
except ImportError:
    websockets = None
    WebSocketClientProtocol = None
    ConnectionClosed = None
    WebSocketException = None

try:
    from PIL import Image
except ImportError:
    Image = None

logger = logging.getLogger(__name__)


class RunnerWebSocketClient:
    """
    WebSocket client for qontinui-runner to communicate with qontinui-web.

    Manages:
    - Authentication and connection
    - Session lifecycle (start/end)
    - Screenshot uploads
    - Log streaming
    - Heartbeat mechanism
    - Error handling and reconnection
    """

    def __init__(
        self,
        api_url: str,
        token: str,
        project_id: str,
        runner_version: str = "0.1.0",
        auto_reconnect: bool = True,
        heartbeat_interval: int = 30,
        max_reconnect_attempts: int = 5,
        on_connected: Optional[Callable] = None,
        on_disconnected: Optional[Callable] = None,
        on_error: Optional[Callable] = None,
    ):
        """
        Initialize WebSocket client.

        Args:
            api_url: WebSocket server URL (e.g., ws://localhost:8001)
            token: JWT authentication token
            project_id: Project UUID
            runner_version: Version of qontinui-runner
            auto_reconnect: Enable automatic reconnection on disconnect
            heartbeat_interval: Seconds between heartbeat messages
            max_reconnect_attempts: Maximum reconnection attempts
            on_connected: Callback when connection established
            on_disconnected: Callback when connection lost
            on_error: Callback for errors
        """
        if websockets is None:
            raise ImportError("websockets library not installed. Install with: pip install websockets")

        self.api_url = api_url
        self.token = token
        self.project_id = project_id
        self.runner_version = runner_version
        self.auto_reconnect = auto_reconnect
        self.heartbeat_interval = heartbeat_interval
        self.max_reconnect_attempts = max_reconnect_attempts

        # Callbacks
        self.on_connected = on_connected
        self.on_disconnected = on_disconnected
        self.on_error = on_error

        # Connection state
        self.ws: Optional[WebSocketClientProtocol] = None
        self.session_id: Optional[str] = None
        self.log_sequence = 0
        self.is_connected = False
        self.is_running = False

        # Tasks
        self.heartbeat_task: Optional[asyncio.Task] = None
        self.reconnect_task: Optional[asyncio.Task] = None

        # System info
        self.runner_os = self._get_os_info()
        self.runner_hostname = socket.gethostname()

        # Statistics
        self.screenshots_sent = 0
        self.logs_sent = 0
        self.heartbeats_sent = 0
        self.reconnect_attempts = 0

    def _get_os_info(self) -> str:
        """Get operating system information."""
        system = platform.system()
        release = platform.release()
        return f"{system} {release}"

    def _get_timestamp(self) -> str:
        """Get current UTC timestamp in ISO format."""
        return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")

    async def connect(self) -> bool:
        """
        Establish WebSocket connection.

        Returns:
            True if connection successful, False otherwise
        """
        try:
            ws_url = f"{self.api_url}/api/v1/automation/ws/automation/runner?token={self.token}"
            logger.info(f"Connecting to qontinui-web: {ws_url}")

            self.ws = await websockets.connect(
                ws_url,
                ping_interval=20,
                ping_timeout=10,
                close_timeout=5,
            )

            self.is_connected = True
            self.is_running = True
            self.reconnect_attempts = 0

            logger.info("WebSocket connection established")

            if self.on_connected:
                try:
                    self.on_connected()
                except Exception as e:
                    logger.error(f"Error in on_connected callback: {e}")

            return True

        except Exception as e:
            logger.error(f"Failed to connect to WebSocket: {e}")
            self.is_connected = False

            if self.on_error:
                try:
                    self.on_error(f"Connection failed: {e}")
                except Exception as cb_error:
                    logger.error(f"Error in on_error callback: {cb_error}")

            return False

    async def disconnect(self):
        """Close WebSocket connection gracefully."""
        logger.info("Disconnecting WebSocket...")
        self.is_running = False

        # Cancel heartbeat task
        if self.heartbeat_task and not self.heartbeat_task.done():
            self.heartbeat_task.cancel()
            try:
                await self.heartbeat_task
            except asyncio.CancelledError:
                pass

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

        logger.info("WebSocket disconnected")

    async def _send_message(self, message: Dict[str, Any]) -> Optional[Dict[str, Any]]:
        """
        Send message to WebSocket server and wait for response.

        Args:
            message: Message dictionary

        Returns:
            Response dictionary if successful, None otherwise
        """
        if not self.is_connected or not self.ws:
            logger.error("Cannot send message: Not connected")
            return None

        try:
            # Send message
            await self.ws.send(json.dumps(message))

            # Wait for response (with timeout)
            response_raw = await asyncio.wait_for(self.ws.recv(), timeout=10.0)
            response = json.loads(response_raw)

            # Check for error response
            if response.get("type") == "error":
                error_msg = response.get("error", "Unknown error")
                details = response.get("details", {})
                logger.error(f"Server error: {error_msg} - {details}")

                if self.on_error:
                    try:
                        self.on_error(f"{error_msg}: {details}")
                    except Exception as e:
                        logger.error(f"Error in on_error callback: {e}")

                return None

            return response

        except asyncio.TimeoutError:
            logger.error("Timeout waiting for server response")
            return None
        except ConnectionClosed:
            logger.error("Connection closed while sending message")
            self.is_connected = False
            if self.auto_reconnect:
                asyncio.create_task(self._reconnect())
            return None
        except Exception as e:
            logger.error(f"Error sending message: {e}")
            return None

    async def start_session(
        self,
        configuration_snapshot: Optional[Dict[str, Any]] = None
    ) -> bool:
        """
        Start automation session.

        Args:
            configuration_snapshot: Optional copy of automation configuration

        Returns:
            True if session started successfully, False otherwise
        """
        message = {
            "type": "session_start",
            "project_id": self.project_id,
            "runner_version": self.runner_version,
            "runner_os": self.runner_os,
            "runner_hostname": self.runner_hostname,
            "configuration_snapshot": configuration_snapshot,
            "timestamp": self._get_timestamp(),
        }

        logger.info(f"Starting session for project: {self.project_id}")
        response = await self._send_message(message)

        if response and response.get("success"):
            self.session_id = response.get("data", {}).get("session_id")
            self.log_sequence = 0
            logger.info(f"Session started successfully: {self.session_id}")

            # Start heartbeat task
            if self.heartbeat_task is None or self.heartbeat_task.done():
                self.heartbeat_task = asyncio.create_task(self._heartbeat_loop())

            return True
        else:
            logger.error("Failed to start session")
            return False

    async def end_session(
        self,
        status: str = "completed",
        error_message: Optional[str] = None
    ) -> bool:
        """
        End automation session.

        Args:
            status: Session status ("completed" or "failed")
            error_message: Optional error message if status is "failed"

        Returns:
            True if session ended successfully, False otherwise
        """
        if not self.session_id:
            logger.warning("Cannot end session: No active session")
            return False

        message = {
            "type": "session_end",
            "session_id": self.session_id,
            "status": status,
            "error_message": error_message,
            "timestamp": self._get_timestamp(),
        }

        logger.info(f"Ending session: {self.session_id} with status: {status}")
        response = await self._send_message(message)

        # Cancel heartbeat task
        if self.heartbeat_task and not self.heartbeat_task.done():
            self.heartbeat_task.cancel()
            try:
                await self.heartbeat_task
            except asyncio.CancelledError:
                pass

        success = response and response.get("success", False)

        if success:
            logger.info("Session ended successfully")
        else:
            logger.error("Failed to end session")

        self.session_id = None
        self.log_sequence = 0

        return success

    async def send_screenshot(
        self,
        image,
        name: str,
        metadata: Optional[Dict[str, Any]] = None
    ) -> Optional[str]:
        """
        Send screenshot to server.

        Args:
            image: PIL Image or bytes or base64 string
            name: Screenshot name
            metadata: Optional automation metadata

        Returns:
            Screenshot ID if successful, None otherwise
        """
        if not self.session_id:
            logger.warning("Cannot send screenshot: No active session")
            return None

        try:
            # Convert image to base64
            if Image and isinstance(image, Image.Image):
                # PIL Image
                buffer = BytesIO()
                image.save(buffer, format="PNG")
                base64_data = base64.b64encode(buffer.getvalue()).decode('utf-8')
                width, height = image.size
                content_type = "image/png"
            elif isinstance(image, bytes):
                # Raw bytes
                base64_data = base64.b64encode(image).decode('utf-8')
                # Try to get dimensions from PIL
                if Image:
                    img = Image.open(BytesIO(image))
                    width, height = img.size
                else:
                    width, height = 0, 0
                content_type = "image/png"
            elif isinstance(image, str):
                # Assume already base64 encoded
                base64_data = image
                width, height = 0, 0
                content_type = "image/png"
            else:
                logger.error(f"Unsupported image type: {type(image)}")
                return None

            message = {
                "type": "screenshot",
                "session_id": self.session_id,
                "screenshot_data": base64_data,
                "name": name,
                "width": width,
                "height": height,
                "content_type": content_type,
                "automation_metadata": metadata or {},
                "timestamp": self._get_timestamp(),
            }

            logger.debug(f"Sending screenshot: {name}")
            response = await self._send_message(message)

            if response and response.get("success"):
                screenshot_id = response.get("data", {}).get("screenshot_id")
                self.screenshots_sent += 1
                logger.debug(f"Screenshot uploaded successfully: {screenshot_id}")
                return screenshot_id
            else:
                logger.error(f"Failed to upload screenshot: {name}")
                return None

        except Exception as e:
            logger.error(f"Error sending screenshot: {e}")
            return None

    async def send_log(
        self,
        level: str,
        message: str,
        log_data: Optional[Dict[str, Any]] = None
    ) -> bool:
        """
        Send log entry to server.

        Args:
            level: Log level (debug, info, warning, error, critical)
            message: Log message
            log_data: Optional structured log data

        Returns:
            True if log sent successfully, False otherwise
        """
        if not self.session_id:
            logger.warning("Cannot send log: No active session")
            return False

        log_message = {
            "type": "log",
            "session_id": self.session_id,
            "level": level,
            "message": message,
            "log_data": log_data or {},
            "sequence_number": self.log_sequence,
            "timestamp": self._get_timestamp(),
        }

        try:
            # Send without waiting for response to avoid blocking
            if self.ws and self.is_connected:
                await self.ws.send(json.dumps(log_message))
                self.log_sequence += 1
                self.logs_sent += 1
                return True
            else:
                return False
        except Exception as e:
            logger.error(f"Error sending log: {e}")
            return False

    async def send_heartbeat(self) -> bool:
        """
        Send heartbeat to keep session alive.

        Returns:
            True if heartbeat successful, False otherwise
        """
        if not self.session_id:
            return False

        message = {
            "type": "heartbeat",
            "session_id": self.session_id,
            "timestamp": self._get_timestamp(),
        }

        try:
            response = await self._send_message(message)
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
        logger.info(f"Starting heartbeat loop (interval: {self.heartbeat_interval}s)")

        try:
            while self.is_running and self.session_id:
                await asyncio.sleep(self.heartbeat_interval)

                if self.session_id and self.is_connected:
                    success = await self.send_heartbeat()
                    if not success:
                        logger.warning("Heartbeat failed, connection may be lost")

        except asyncio.CancelledError:
            logger.info("Heartbeat loop cancelled")
        except Exception as e:
            logger.error(f"Error in heartbeat loop: {e}")

    async def _reconnect(self):
        """Attempt to reconnect to WebSocket server."""
        if self.reconnect_task and not self.reconnect_task.done():
            # Already reconnecting
            return

        logger.info("Attempting to reconnect...")

        while self.reconnect_attempts < self.max_reconnect_attempts:
            self.reconnect_attempts += 1
            wait_time = min(2 ** self.reconnect_attempts, 60)  # Exponential backoff

            logger.info(f"Reconnection attempt {self.reconnect_attempts}/{self.max_reconnect_attempts} "
                       f"in {wait_time}s...")

            await asyncio.sleep(wait_time)

            success = await self.connect()
            if success:
                logger.info("Reconnection successful")

                # Try to resume session (implementation depends on server support)
                if self.session_id:
                    logger.info(f"Resuming session: {self.session_id}")
                    # Session should still be valid on server side
                    # Restart heartbeat
                    if self.heartbeat_task is None or self.heartbeat_task.done():
                        self.heartbeat_task = asyncio.create_task(self._heartbeat_loop())

                return

        logger.error(f"Failed to reconnect after {self.max_reconnect_attempts} attempts")

        if self.on_error:
            try:
                self.on_error("Max reconnection attempts reached")
            except Exception as e:
                logger.error(f"Error in on_error callback: {e}")

    def get_stats(self) -> Dict[str, Any]:
        """
        Get client statistics.

        Returns:
            Dictionary with client statistics
        """
        return {
            "is_connected": self.is_connected,
            "session_id": self.session_id,
            "screenshots_sent": self.screenshots_sent,
            "logs_sent": self.logs_sent,
            "heartbeats_sent": self.heartbeats_sent,
            "reconnect_attempts": self.reconnect_attempts,
        }


# Helper function for authentication
async def authenticate(api_url: str, email: str, password: str) -> Optional[str]:
    """
    Authenticate with qontinui-web and get JWT token.

    Args:
        api_url: API base URL (e.g., http://localhost:8001)
        email: User email
        password: User password

    Returns:
        JWT access token if successful, None otherwise
    """
    try:
        import requests
    except ImportError:
        logger.error("requests library not installed. Install with: pip install requests")
        return None

    try:
        auth_url = f"{api_url}/api/v1/auth/login"

        logger.info(f"Authenticating with {auth_url}")

        response = requests.post(
            auth_url,
            json={"email": email, "password": password},
            timeout=10,
        )

        if response.status_code == 200:
            data = response.json()
            token = data.get("access_token")
            logger.info("Authentication successful")
            return token
        else:
            logger.error(f"Authentication failed: {response.status_code} - {response.text}")
            return None

    except Exception as e:
        logger.error(f"Authentication error: {e}")
        return None
