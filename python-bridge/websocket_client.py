"""
WebSocket client for qontinui-runner to communicate with qontinui-web backend.

This module provides real-time bidirectional communication for:
- Live screenshot streaming
- Automation log streaming
- Session tracking with heartbeats
- Connection management with auto-reconnection
"""

import asyncio
import base64
import contextlib
import json
import logging
import os
import platform
import socket
import sys
from collections.abc import Callable
from io import BytesIO
from typing import Any

from qontinui_schemas.common import utc_now

# Debug log file path - hardcoded to ensure it works
_WS_CLIENT_DEBUG_LOG = (
    r"C:\Users\Joshua\Documents\qontinui_parent_directory\.dev-logs\ws-client-debug.log"
)


def _debug_log(message: str) -> None:
    """Write debug message to log file."""
    try:
        import datetime as dt

        with open(_WS_CLIENT_DEBUG_LOG, "a") as f:
            f.write(f"[{dt.datetime.now().isoformat()}] {message}\n")
            f.flush()
    except Exception as e:
        # Write exception to stderr as fallback
        print(f"[ERROR] _debug_log failed: {e}", file=sys.stderr, flush=True)


# Write initial log to verify file creation works
_debug_log("websocket_client module loaded")

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
        runner_name: str | None = None,
        auto_reconnect: bool = True,
        heartbeat_interval: int = 30,
        max_reconnect_attempts: int = 5,
        on_connected: Callable | None = None,
        on_disconnected: Callable | None = None,
        on_error: Callable | None = None,
    ):
        """
        Initialize WebSocket client.

        Args:
            api_url: WebSocket server URL (e.g., ws://localhost:8000)
            token: JWT authentication token
            project_id: Project UUID
            runner_version: Version of qontinui-runner
            runner_name: Custom user-defined name for this runner
            auto_reconnect: Enable automatic reconnection on disconnect
            heartbeat_interval: Seconds between heartbeat messages
            max_reconnect_attempts: Maximum reconnection attempts
            on_connected: Callback when connection established
            on_disconnected: Callback when connection lost
            on_error: Callback for errors
        """
        if websockets is None:
            raise ImportError(
                "websockets library not installed. Install with: pip install websockets"
            )

        self.api_url = api_url
        self.token = token
        self.project_id = project_id
        self.runner_version = runner_version
        self.runner_name = runner_name
        self.auto_reconnect = auto_reconnect
        self.heartbeat_interval = heartbeat_interval
        self.max_reconnect_attempts = max_reconnect_attempts

        # Callbacks
        self.on_connected = on_connected
        self.on_disconnected = on_disconnected
        self.on_error = on_error
        self.on_command: Callable[[dict[str, Any]], None] | None = None

        # Connection state
        self.ws: WebSocketClientProtocol | None = None
        self.session_id: str | None = None
        self.log_sequence = 0
        self.is_connected = False
        self.is_running = False

        # Tasks
        self.heartbeat_task: asyncio.Task | None = None
        self.reconnect_task: asyncio.Task | None = None
        self.listener_task: asyncio.Task | None = None

        # Response queue for request-response pattern - keyed by expected response type
        self._pending_responses: dict[str, asyncio.Queue] = {}
        self._response_lock = asyncio.Lock()
        # Mapping of request types to expected response types
        self._request_response_map = {
            "session_start": "session_started",
            "session_end": "session_ended",
        }

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
        return utc_now().isoformat().replace("+00:00", "Z")

    async def connect(self) -> bool:
        """
        Establish WebSocket connection.

        Returns:
            True if connection successful, False otherwise
        """
        # DEBUG: Write to file for visibility regardless of how runner is started
        from datetime import datetime

        # Path: websocket_client.py -> python-bridge -> qontinui-runner -> qontinui_parent_directory
        debug_log_path = os.path.join(
            os.path.dirname(os.path.dirname(os.path.dirname(__file__))),
            ".dev-logs",
            "python-ws-debug.log",
        )
        try:
            with open(debug_log_path, "a") as f:
                f.write(f"\n[{datetime.now().isoformat()}] WebSocket connect() called\n")
                f.write(f"  api_url: {self.api_url}\n")
                f.write(f"  token_len: {len(self.token) if self.token else 0}\n")
                f.flush()
        except Exception as e:
            logger.warning(f"Could not write debug log: {e}")

        try:
            # Extract just the host:port from api_url in case it already has a path
            # This prevents URL duplication bugs if api_url is "ws://host:port/some/path"
            from urllib.parse import urlparse

            parsed = urlparse(self.api_url)
            base_url = f"{parsed.scheme}://{parsed.netloc}"

            ws_url = f"{base_url}/api/v1/automation/ws/automation/runner?token={self.token}"
            logger.info(f"Connecting to qontinui-web: {ws_url}")

            # DEBUG: Log the full URL being used
            try:
                with open(debug_log_path, "a") as f:
                    f.write(f"  base_url (extracted): {base_url}\n")
                    f.write(f"  full_ws_url: {ws_url[:120]}...\n")
                    f.flush()
            except Exception:
                pass

            self.ws = await websockets.connect(  # type: ignore[assignment]
                ws_url,
                ping_interval=20,
                ping_timeout=10,
                close_timeout=5,
            )

            # DEBUG: Log successful connection
            try:
                with open(debug_log_path, "a") as f:
                    f.write("  CONNECTION SUCCESS!\n")
                    f.flush()
            except Exception:
                pass

            self.is_connected = True
            self.is_running = True
            self.reconnect_attempts = 0

            logger.info("WebSocket connection established")

            # Send runner info immediately after connection
            # This allows the backend to update the connection record with the runner name
            await self._send_runner_info()

            # Start listener task to receive incoming commands
            if self.listener_task is None or self.listener_task.done():
                self.listener_task = asyncio.create_task(self._message_listener())

            if self.on_connected:
                try:
                    self.on_connected()
                except Exception as e:
                    logger.error(f"Error in on_connected callback: {e}")

            return True

        except Exception as e:
            logger.error(f"Failed to connect to WebSocket: {e}")
            self.is_connected = False

            # DEBUG: Log connection failure
            try:
                with open(debug_log_path, "a") as f:
                    f.write(f"  CONNECTION FAILED: {e}\n")
                    f.write(f"  Error type: {type(e).__name__}\n")
                    f.flush()
            except Exception:
                pass

            if self.on_error:
                try:
                    self.on_error(f"Connection failed: {e}")
                except Exception as cb_error:
                    logger.error(f"Error in on_error callback: {cb_error}")

            return False

    async def disconnect(self):
        """Close WebSocket connection gracefully."""
        logger.info("Disconnecting WebSocket...")

        # Disable auto-reconnect FIRST to prevent reconnection attempts
        self.auto_reconnect = False
        self.is_running = False

        # Cancel reconnect task if running
        if self.reconnect_task and not self.reconnect_task.done():
            self.reconnect_task.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await self.reconnect_task
            self.reconnect_task = None

        # Cancel heartbeat task
        if self.heartbeat_task and not self.heartbeat_task.done():
            self.heartbeat_task.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await self.heartbeat_task

        # Cancel listener task
        if self.listener_task and not self.listener_task.done():
            self.listener_task.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await self.listener_task

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

    async def _send_message(self, message: dict[str, Any]) -> dict[str, Any] | None:
        """
        Send message to WebSocket server and wait for response.

        Args:
            message: Message dictionary

        Returns:
            Response dictionary if successful, None otherwise
        """
        _debug_log(f"_send_message: is_connected={self.is_connected}, ws={self.ws is not None}")
        if not self.is_connected or not self.ws:
            _debug_log("Cannot send message: Not connected")
            logger.error("Cannot send message: Not connected")
            return None

        try:
            # Determine expected response type based on request type
            request_type = message.get("type")
            expected_response_type = self._request_response_map.get(request_type)

            # Create a response queue keyed by expected response type
            response_queue: asyncio.Queue = asyncio.Queue()
            if expected_response_type:
                async with self._response_lock:
                    self._pending_responses[expected_response_type] = response_queue

            try:
                # Send message
                _debug_log(
                    f"_send_message: sending type={request_type}, expecting={expected_response_type}"
                )
                await self.ws.send(json.dumps(message))  # type: ignore[attr-defined]

                if not expected_response_type:
                    # No response expected
                    return {"success": True}

                # Wait for response from the message listener (with timeout)
                _debug_log("_send_message: waiting for response via queue...")
                response = await asyncio.wait_for(response_queue.get(), timeout=10.0)
                _debug_log(
                    f"_send_message: received response type={response.get('type')}, success={response.get('success')}"
                )

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

                return response  # type: ignore[no-any-return]
            finally:
                # Clean up the pending response entry
                if expected_response_type:
                    async with self._response_lock:
                        self._pending_responses.pop(expected_response_type, None)

        except TimeoutError:
            _debug_log("_send_message: TIMEOUT waiting for server response")
            logger.error("Timeout waiting for server response")
            return None
        except ConnectionClosed as e:
            _debug_log(f"_send_message: ConnectionClosed error: {e}")
            logger.error("Connection closed while sending message")
            self.is_connected = False
            if self.auto_reconnect:
                asyncio.create_task(self._reconnect())
            return None
        except Exception as e:
            _debug_log(f"_send_message: Exception: {type(e).__name__}: {e}")
            logger.error(f"Error sending message: {e}")
            return None

    async def send_message(self, message: dict[str, Any]) -> bool:
        """
        Send message to WebSocket server without waiting for response.

        This is used for sending arbitrary message types like extraction events
        where we don't need to wait for a specific response.

        Args:
            message: Message dictionary

        Returns:
            True if message sent successfully, False otherwise
        """
        if not self.is_connected or not self.ws:
            logger.error("Cannot send message: Not connected")
            return False

        try:
            await self.ws.send(json.dumps(message))  # type: ignore[attr-defined]
            logger.debug(f"Message sent: type={message.get('type')}")
            return True
        except ConnectionClosed:
            logger.error("Connection closed while sending message")
            self.is_connected = False
            if self.auto_reconnect:
                asyncio.create_task(self._reconnect())
            return False
        except Exception as e:
            logger.error(f"Error sending message: {e}")
            return False

    async def _send_runner_info(self) -> bool:
        """
        Send runner information immediately after connection.

        This allows the backend to update the connection record with
        the custom runner name and other metadata.

        Returns:
            True if info sent successfully, False otherwise
        """
        # Message format must match WSMessage schema: {type, data}
        message = {
            "type": "runner_info",
            "data": {
                "runner_name": self.runner_name,
                "runner_version": self.runner_version,
                "runner_os": self.runner_os,
                "runner_hostname": self.runner_hostname,
                "timestamp": self._get_timestamp(),
            },
        }

        logger.info(f"Sending runner info: name={self.runner_name}")

        try:
            if self.ws and self.is_connected:
                await self.ws.send(json.dumps(message))  # type: ignore[attr-defined]
                logger.info("Runner info sent successfully")
                return True
        except Exception as e:
            logger.error(f"Error sending runner info: {e}")

        return False

    async def start_session(self, configuration_snapshot: dict[str, Any] | None = None) -> bool:
        """
        Start automation session.

        Args:
            configuration_snapshot: Optional copy of automation configuration

        Returns:
            True if session started successfully, False otherwise
        """
        # Generate a session_id for this automation run
        import uuid as uuid_module

        generated_session_id = str(uuid_module.uuid4())

        message = {
            "type": "session_start",
            "data": {
                "project_id": self.project_id,
                "session_id": generated_session_id,  # Provide session_id for backend to track
                "runner_version": self.runner_version,
                "runner_os": self.runner_os,
                "runner_hostname": self.runner_hostname,
                "runner_name": self.runner_name,
                "configuration_snapshot": configuration_snapshot,
                "timestamp": self._get_timestamp(),
            },
        }

        logger.info(f"Starting session for project: {self.project_id}")
        _debug_log(f"start_session: is_connected={self.is_connected}, ws={self.ws is not None}")
        response = await self._send_message(message)
        _debug_log(f"start_session: _send_message returned: {response}")

        # Check for success: either explicit success=True or type="session_started"
        if response and (response.get("success") or response.get("type") == "session_started"):
            # Use session_id from response, or fall back to generated one
            response_session_id = response.get("data", {}).get("session_id")
            self.session_id = response_session_id or generated_session_id
            self.log_sequence = 0
            logger.info(f"Session started successfully: {self.session_id}")
            _debug_log(
                f"start_session: SUCCESS session_id={self.session_id} (from response={response_session_id})"
            )

            # Start heartbeat task
            if self.heartbeat_task is None or self.heartbeat_task.done():
                self.heartbeat_task = asyncio.create_task(self._heartbeat_loop())

            return True
        else:
            _debug_log(f"start_session: FAILED response={response}")
            logger.error(f"Failed to start session: response={response}")
            return False

    async def end_session(
        self, status: str = "completed", error_message: str | None = None
    ) -> bool:
        """
        End automation session.

        Args:
            status: Session status ("completed" or "failed")
            error_message: Optional error message if status is "failed"

        Returns:
            True if session ended successfully, False otherwise
        """
        _debug_log(f"end_session: called with status={status}, session_id={self.session_id}")

        if not self.session_id:
            logger.warning("Cannot end session: No active session")
            _debug_log("end_session: No active session")
            return False

        message = {
            "type": "session_end",
            "data": {
                "session_id": self.session_id,
                "status": status,
                "error_message": error_message,
                "timestamp": self._get_timestamp(),
            },
        }

        logger.info(f"Ending session: {self.session_id} with status: {status}")
        _debug_log("end_session: sending session_end message")
        response = await self._send_message(message)

        # Cancel heartbeat task
        if self.heartbeat_task and not self.heartbeat_task.done():
            self.heartbeat_task.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await self.heartbeat_task

        # Check for success: either explicit success=True or type="session_ended"
        success = response and (response.get("success") or response.get("type") == "session_ended")

        if success:
            logger.info("Session ended successfully")
            _debug_log("end_session: SUCCESS")
        else:
            logger.error(f"Failed to end session: response={response}")
            _debug_log(f"end_session: FAILED response={response}")

        self.session_id = None
        self.log_sequence = 0

        return success  # type: ignore[return-value]

    async def send_screenshot(
        self, image, name: str, metadata: dict[str, Any] | None = None
    ) -> str | None:
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
                base64_data = base64.b64encode(buffer.getvalue()).decode("utf-8")
                width, height = image.size
                content_type = "image/png"
            elif isinstance(image, bytes):
                # Raw bytes
                base64_data = base64.b64encode(image).decode("utf-8")
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

            # Note: Backend expects "data.image" format for the screenshot
            # along with "data.metadata" for all metadata
            message = {
                "type": "screenshot",
                "data": {
                    "image": base64_data,
                    "metadata": {
                        "name": name,
                        "width": width,
                        "height": height,
                        "content_type": content_type,
                        **(metadata or {}),
                    },
                },
            }

            logger.debug(f"Sending screenshot: {name}")
            response = await self._send_message(message)

            if response and response.get("success"):
                screenshot_id = response.get("data", {}).get("screenshot_id")
                self.screenshots_sent += 1
                logger.debug(f"Screenshot uploaded successfully: {screenshot_id}")
                return screenshot_id  # type: ignore[no-any-return]
            else:
                logger.error(f"Failed to upload screenshot: {name}")
                return None

        except Exception as e:
            logger.error(f"Error sending screenshot: {e}")
            return None

    async def send_log(
        self, level: str, message: str, log_data: dict[str, Any] | None = None
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
            "data": {
                "session_id": self.session_id,
                "level": level,
                "message": message,
                "data": log_data or {},
                "sequence_number": self.log_sequence,
                "timestamp": self._get_timestamp(),
            },
        }

        try:
            # Send without waiting for response to avoid blocking
            if self.ws and self.is_connected:
                await self.ws.send(json.dumps(log_message))  # type: ignore[attr-defined]
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
            "data": {
                "session_id": self.session_id,
                "timestamp": self._get_timestamp(),
            },
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

    async def _message_listener(self):
        """
        Background task to listen for incoming messages from the server.

        This handles commands sent from the frontend through the backend.
        Commands have the format: {"type": "command", "command": "...", "params": {...}}
        """
        logger.info("Starting message listener loop")

        try:
            while self.is_running and self.is_connected and self.ws:
                try:
                    # Wait for incoming message
                    message_raw = await self.ws.recv()
                    message = json.loads(message_raw)

                    message_type = message.get("type")
                    request_id = message.get("request_id")
                    _debug_log(
                        f"_message_listener: received type={message_type}, request_id={request_id}"
                    )
                    logger.debug(f"Received message: type={message_type}")

                    if message_type == "command":
                        # Handle command from frontend
                        command = message.get("command")
                        # Don't log high-frequency commands to avoid flooding
                        if command not in ("ping", "status"):
                            logger.info(f"Received command: {command}")

                        if self.on_command:
                            try:
                                self.on_command(message)
                            except Exception as e:
                                logger.error(f"Error in on_command callback: {e}")
                        else:
                            logger.warning(
                                f"No on_command handler registered for command: {command}"
                            )

                    elif message_type == "ping":
                        # Respond to server ping
                        try:
                            await self.ws.send(
                                json.dumps(
                                    {
                                        "type": "pong",
                                        "timestamp": self._get_timestamp(),
                                    }
                                )
                            )
                        except Exception as e:
                            logger.error(f"Error sending pong: {e}")

                    elif message_type in [
                        "heartbeat_ack",
                        "session_started",
                        "session_ended",
                        "log_received",
                        "screenshot_stored",
                        "connected",
                        "pong",
                        "runner_info_ack",
                    ]:
                        # Check if there's a pending request waiting for this response type
                        if message_type in self._pending_responses:
                            _debug_log(
                                f"_message_listener: dispatching {message_type} to waiting caller"
                            )
                            await self._pending_responses[message_type].put(message)
                        else:
                            # Unsolicited response - log it but don't process
                            logger.debug(f"Received unsolicited response type={message_type}")

                    elif message_type == "error":
                        # Check if there's any pending request that might be waiting
                        # For errors, we dispatch to any pending response queue since we don't know
                        # which request triggered the error
                        dispatched = False
                        for response_type, queue in list(self._pending_responses.items()):
                            _debug_log(
                                f"_message_listener: dispatching error to pending {response_type}"
                            )
                            await queue.put(message)
                            dispatched = True
                            break  # Only dispatch to one waiter
                        if not dispatched:
                            error_msg = message.get("message", "Unknown error")
                            logger.error(f"Server error: {error_msg}")
                            if self.on_error:
                                try:
                                    self.on_error(error_msg)
                                except Exception as e:
                                    logger.error(f"Error in on_error callback: {e}")

                    else:
                        logger.debug(f"Unhandled message type: {message_type}")

                except TimeoutError:
                    # No message received, continue loop
                    continue

                except ConnectionClosed:
                    logger.warning("Connection closed in listener")
                    self.is_connected = False
                    if self.auto_reconnect and self.is_running:
                        asyncio.create_task(self._reconnect())
                    break

                except Exception as e:
                    logger.error(f"Error in message listener: {e}")
                    if not self.is_running:
                        break
                    await asyncio.sleep(0.1)  # Brief pause before retrying

        except asyncio.CancelledError:
            logger.info("Message listener cancelled")
        except Exception as e:
            logger.error(f"Fatal error in message listener: {e}")

        logger.info("Message listener stopped")

    async def _reconnect(self):
        """Attempt to reconnect to WebSocket server."""
        # Don't reconnect if explicitly stopped
        if not self.is_running or not self.auto_reconnect:
            logger.info("Reconnection skipped - client is stopped or auto-reconnect disabled")
            return

        if self.reconnect_task and not self.reconnect_task.done():
            # Already reconnecting
            return

        logger.info("Attempting to reconnect...")

        while self.reconnect_attempts < self.max_reconnect_attempts and self.is_running:
            self.reconnect_attempts += 1
            wait_time = min(2**self.reconnect_attempts, 60)  # Exponential backoff

            logger.info(
                f"Reconnection attempt {self.reconnect_attempts}/{self.max_reconnect_attempts} "
                f"in {wait_time}s..."
            )

            await asyncio.sleep(wait_time)

            # Check again after sleep in case disconnect was called
            if not self.is_running or not self.auto_reconnect:
                logger.info("Reconnection cancelled during wait")
                return

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

    async def send_issue_detected(self, issue_data: dict[str, Any]) -> bool:
        """
        Send a detected issue to the server.

        This is called when the IssueTracker in the frontend detects a new issue
        from AI output parsing.

        Args:
            issue_data: Issue data matching DetectedIssue schema:
                - id: str (client-generated UUID)
                - type: str (error, warning, exception, type_error, runtime_error)
                - severity: str (critical, high, medium, low)
                - title: str
                - description: str | None
                - file: str | None
                - line: int | None
                - source: dict (type, path, line_range, description)
                - status: str (detected, in_progress, resolved, skipped)
                - detected_at: str (ISO timestamp)

        Returns:
            True if sent successfully, False otherwise
        """
        if not self.session_id:
            logger.warning("Cannot send issue: No active session")
            return False

        message = {
            "type": "issue_detected",
            "data": {
                "session_id": self.session_id,
                "project_id": self.project_id,
                "payload": issue_data,
                "timestamp": self._get_timestamp(),
            },
        }

        logger.info(f"Sending detected issue: {issue_data.get('title', 'Unknown')[:50]}")

        try:
            return await self.send_message(message)
        except Exception as e:
            logger.error(f"Error sending issue_detected: {e}")
            return False

    async def send_issue_updated(
        self,
        issue_id: str,
        status: str,
        resolution: str | None = None,
    ) -> bool:
        """
        Send an issue status update to the server.

        This is called when an issue status changes (e.g., marked as resolved).

        Args:
            issue_id: The issue ID
            status: New status (detected, in_progress, resolved, skipped)
            resolution: Optional resolution description

        Returns:
            True if sent successfully, False otherwise
        """
        if not self.session_id:
            logger.warning("Cannot send issue update: No active session")
            return False

        message = {
            "type": "issue_updated",
            "data": {
                "session_id": self.session_id,
                "payload": {
                    "id": issue_id,
                    "status": status,
                    "resolution": resolution,
                    "updated_at": self._get_timestamp(),
                },
                "timestamp": self._get_timestamp(),
            },
        }

        logger.info(f"Sending issue update: {issue_id} -> {status}")

        try:
            return await self.send_message(message)
        except Exception as e:
            logger.error(f"Error sending issue_updated: {e}")
            return False

    async def send_issues_sync(self, issues: list[dict[str, Any]]) -> bool:
        """
        Sync all issues to the server (bulk update).

        This is called periodically or when the session ends to ensure
        all issues are persisted to the backend.

        Args:
            issues: List of issue data dictionaries

        Returns:
            True if sent successfully, False otherwise
        """
        if not self.session_id:
            logger.warning("Cannot sync issues: No active session")
            return False

        message = {
            "type": "issues_sync",
            "data": {
                "session_id": self.session_id,
                "project_id": self.project_id,
                "payload": {
                    "issues": issues,
                },
                "timestamp": self._get_timestamp(),
            },
        }

        logger.info(f"Syncing {len(issues)} issues to server")

        try:
            return await self.send_message(message)
        except Exception as e:
            logger.error(f"Error sending issues_sync: {e}")
            return False

    def get_stats(self) -> dict[str, Any]:
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
async def authenticate(api_url: str, email: str, password: str) -> str | None:
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
            return token  # type: ignore[no-any-return]
        else:
            logger.error(f"Authentication failed: {response.status_code} - {response.text}")
            return None

    except Exception as e:
        logger.error(f"Authentication error: {e}")
        return None
