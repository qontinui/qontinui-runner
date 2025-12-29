"""
WebSocket Handler Module

Single Responsibility: Manage WebSocket client communication
- Connect/disconnect from WebSocket server
- Start/end WebSocket sessions
- Send logs and screenshots to WebSocket
- Manage event loop in background thread
"""

import asyncio
import logging
import sys
import threading
import time
import traceback
from collections.abc import Callable
from typing import Any

logger = logging.getLogger(__name__)

# Check if WebSocket modules are available
try:
    from websocket_client import RunnerWebSocketClient, authenticate
    from websocket_config import WebSocketConfig

    WEBSOCKET_AVAILABLE = True
except ImportError:
    WEBSOCKET_AVAILABLE = False
    logger.debug("WebSocket modules not available")


class WebSocketHandler:
    """
    Manages WebSocket client communication.

    Responsibilities:
    - Initialize WebSocket configuration
    - Connect to WebSocket server
    - Manage session lifecycle
    - Send logs and screenshots
    - Run event loop in background thread
    """

    def __init__(
        self,
        emit_log_fn: Callable[[str, str], None],
        on_command_fn: Callable[[dict[str, Any]], None] | None = None,
    ):
        """
        Initialize WebSocket handler.

        Args:
            emit_log_fn: Function to emit log messages (level, message)
            on_command_fn: Optional callback for handling incoming commands from backend
        """
        self.emit_log = emit_log_fn
        self.on_command_fn = on_command_fn
        self.ws_client: RunnerWebSocketClient | None = None
        self.ws_config: WebSocketConfig | None = None
        self.ws_loop: asyncio.AbstractEventLoop | None = None
        self.ws_thread: threading.Thread | None = None
        self.ws_enabled = False
        self._reconnect_lock = threading.Lock()
        self._reconnect_in_progress = False

        # Initialize WebSocket configuration from environment
        if WEBSOCKET_AVAILABLE:
            try:
                self.ws_config = WebSocketConfig.from_env()
                if self.ws_config.enabled:
                    self.emit_log("info", "WebSocket configuration loaded from environment")
            except Exception as e:
                self.emit_log("error", f"Failed to load WebSocket config: {e}")

    def _is_loop_running(self) -> bool:
        """
        Check if the WebSocket event loop is alive and running.

        Returns:
            True if the event loop is running, False otherwise
        """
        if not self.ws_loop:
            return False
        try:
            return self.ws_loop.is_running() and not self.ws_loop.is_closed()
        except Exception:
            return False

    def _mark_disconnected(self, reason: str = "unknown"):
        """
        Mark the WebSocket as disconnected and clean up state.

        Args:
            reason: Reason for disconnection (for logging)
        """
        if self.ws_enabled:
            print(
                f"[warning ] WS_HANDLER: Connection lost ({reason}), marking as disconnected",
                file=sys.stderr,
                flush=True,
            )
            self.emit_log("warning", f"WebSocket connection lost: {reason}")
        self.ws_enabled = False
        # Don't clear ws_config so we can reconnect with same settings

    def configure(
        self,
        enabled: bool,
        api_url: str,
        token: str,
        project_id: str | None = None,
        runner_name: str | None = None,
    ) -> bool:
        """
        Configure WebSocket connection settings.

        Args:
            enabled: Whether WebSocket is enabled
            api_url: WebSocket API URL (e.g., ws://localhost:8000)
            token: JWT authentication token
            project_id: Optional project ID
            runner_name: Optional custom name for this runner

        Returns:
            True if configuration successful, False otherwise
        """
        self.emit_log(
            "info",
            f"[WS_HANDLER] configure called: enabled={enabled}, url={api_url}, project_id={project_id}, runner_name={runner_name}",
        )
        self.emit_log("info", f"[WS_HANDLER] WEBSOCKET_AVAILABLE={WEBSOCKET_AVAILABLE}")

        if not WEBSOCKET_AVAILABLE:
            self.emit_log(
                "error",
                "[WS_HANDLER] WebSocket libraries not available - check websocket_client.py import",
            )
            return False

        try:
            # Create new config from provided values
            self.ws_config = WebSocketConfig(
                enabled=enabled,
                api_url=api_url,
                token=token,
                project_id=str(project_id) if project_id else None,
                runner_name=runner_name,
                auto_reconnect=True,
                heartbeat_interval=30,
                max_reconnect_attempts=5,
            )

            self.emit_log(
                "info",
                f"[WS_HANDLER] WebSocket configured successfully: enabled={enabled}, url={api_url}, project_id={project_id}, runner_name={runner_name}",
            )
            return True

        except Exception as e:
            self.emit_log("error", f"[WS_HANDLER] Failed to configure WebSocket: {e}")
            import traceback

            self.emit_log("error", f"[WS_HANDLER] Traceback: {traceback.format_exc()}")
            return False

    def _start_event_loop(self):
        """Start WebSocket event loop in background thread."""
        try:
            self.ws_loop = asyncio.new_event_loop()
            asyncio.set_event_loop(self.ws_loop)
            self.ws_loop.run_forever()
        except Exception as e:
            logger.error(f"WebSocket event loop error: {e}")
        finally:
            if self.ws_loop:
                self.ws_loop.close()

    def connect(self) -> bool:
        """
        Connect to WebSocket server (blocking until connected).

        Returns:
            True if connection successful, False otherwise
        """
        import sys

        print("[info    ] WS_HANDLER: connect() called", file=sys.stderr, flush=True)
        print(
            f"[info    ] WS_HANDLER: WEBSOCKET_AVAILABLE={WEBSOCKET_AVAILABLE}",
            file=sys.stderr,
            flush=True,
        )
        print(
            f"[info    ] WS_HANDLER: ws_config exists: {self.ws_config is not None}",
            file=sys.stderr,
            flush=True,
        )

        if not WEBSOCKET_AVAILABLE:
            print(
                "[error   ] WS_HANDLER: WebSocket libraries not available",
                file=sys.stderr,
                flush=True,
            )
            return False

        if not self.ws_config:
            print(
                "[error   ] WS_HANDLER: WebSocket configuration not set",
                file=sys.stderr,
                flush=True,
            )
            return False

        print(
            f"[info    ] WS_HANDLER: ws_config.enabled={self.ws_config.enabled}, api_url={self.ws_config.api_url}",
            file=sys.stderr,
            flush=True,
        )

        # Validate config
        is_valid, error = self.ws_config.validate()
        if not is_valid:
            print(
                f"[error   ] WS_HANDLER: Invalid WebSocket config: {error}",
                file=sys.stderr,
                flush=True,
            )
            return False

        print(
            "[info    ] WS_HANDLER: Config validation passed, starting connection...",
            file=sys.stderr,
            flush=True,
        )

        try:
            # Start event loop in background thread
            print(
                "[info    ] WS_HANDLER: Starting event loop thread...", file=sys.stderr, flush=True
            )
            if not self.ws_thread or not self.ws_thread.is_alive():
                self.ws_thread = threading.Thread(target=self._start_event_loop, daemon=True)
                self.ws_thread.start()

                # Wait for loop to be ready
                timeout = 5
                start_time = time.time()
                while not self.ws_loop and time.time() - start_time < timeout:
                    time.sleep(0.1)

                if not self.ws_loop:
                    print(
                        "[error   ] WS_HANDLER: Event loop failed to start",
                        file=sys.stderr,
                        flush=True,
                    )
                    return False

            print("[info    ] WS_HANDLER: Event loop ready", file=sys.stderr, flush=True)

            # Get or refresh token
            token = self.ws_config.token
            print(
                f"[info    ] WS_HANDLER: token_len={len(token) if token else 0}",
                file=sys.stderr,
                flush=True,
            )
            if not token and self.ws_config.email and self.ws_config.password:
                print(
                    "[info    ] WS_HANDLER: Authenticating with qontinui-web...",
                    file=sys.stderr,
                    flush=True,
                )
                # Convert ws:// to http:// for auth
                auth_url = self.ws_config.api_url.replace("ws://", "http://").replace(
                    "wss://", "https://"
                )

                # Run auth in event loop
                future = asyncio.run_coroutine_threadsafe(
                    authenticate(auth_url, self.ws_config.email, self.ws_config.password),
                    self.ws_loop,  # type: ignore[arg-type]
                )
                token = future.result(timeout=10)

                if not token:
                    print(
                        "[error   ] WS_HANDLER: Authentication failed", file=sys.stderr, flush=True
                    )
                    return False

                self.ws_config.token = token
                print(
                    "[info    ] WS_HANDLER: Authentication successful", file=sys.stderr, flush=True
                )

            # Create WebSocket client
            print(
                "[info    ] WS_HANDLER: Creating WebSocket client...", file=sys.stderr, flush=True
            )
            print(
                f"[info    ] WS_HANDLER: api_url={self.ws_config.api_url}, project_id={self.ws_config.project_id}, runner_name={self.ws_config.runner_name}",
                file=sys.stderr,
                flush=True,
            )
            self.ws_client = RunnerWebSocketClient(
                api_url=self.ws_config.api_url,
                token=token,  # type: ignore[arg-type]
                project_id=self.ws_config.project_id,  # type: ignore[arg-type]
                runner_version=self.ws_config.runner_version,
                runner_name=self.ws_config.runner_name,
                auto_reconnect=self.ws_config.auto_reconnect,
                heartbeat_interval=self.ws_config.heartbeat_interval,
                max_reconnect_attempts=self.ws_config.max_reconnect_attempts,
                on_connected=lambda: print(
                    "[info    ] WS_HANDLER: WebSocket connected callback",
                    file=sys.stderr,
                    flush=True,
                ),
                on_disconnected=lambda: print(
                    "[warning ] WS_HANDLER: WebSocket disconnected callback",
                    file=sys.stderr,
                    flush=True,
                ),
                on_error=lambda msg: print(
                    f"[error   ] WS_HANDLER: WebSocket error callback: {msg}",
                    file=sys.stderr,
                    flush=True,
                ),
            )

            # Set command handler for incoming commands from backend
            if self.on_command_fn:
                self.ws_client.on_command = self._handle_command
                print(
                    "[info    ] WS_HANDLER: Command handler registered",
                    file=sys.stderr,
                    flush=True,
                )

            # Connect
            print(
                "[info    ] WS_HANDLER: Calling ws_client.connect()...",
                file=sys.stderr,
                flush=True,
            )
            future = asyncio.run_coroutine_threadsafe(self.ws_client.connect(), self.ws_loop)  # type: ignore[arg-type]
            success = future.result(timeout=10)
            print(
                f"[info    ] WS_HANDLER: ws_client.connect() returned: {success}",
                file=sys.stderr,
                flush=True,
            )

            if success:
                self.ws_enabled = True
                print(
                    "[info    ] WS_HANDLER: WebSocket connection established!",
                    file=sys.stderr,
                    flush=True,
                )
                return True
            else:
                print(
                    "[error   ] WS_HANDLER: WebSocket connection failed",
                    file=sys.stderr,
                    flush=True,
                )
                return False

        except Exception as e:
            print(f"[error   ] WS_HANDLER: Exception: {e}", file=sys.stderr, flush=True)
            print(
                f"[error   ] WS_HANDLER: Traceback: {traceback.format_exc()}",
                file=sys.stderr,
                flush=True,
            )
            return False

    def disconnect(self):
        """Disconnect from WebSocket server."""
        if not self.ws_client:
            return

        try:
            self.emit_log("info", "Disconnecting WebSocket...")

            # Disconnect client
            if self.ws_loop and self.ws_client:
                future = asyncio.run_coroutine_threadsafe(self.ws_client.disconnect(), self.ws_loop)
                future.result(timeout=5)

            # Stop event loop
            if self.ws_loop:
                self.ws_loop.call_soon_threadsafe(self.ws_loop.stop)

            self.ws_enabled = False
            self.ws_client = None

            self.emit_log("info", "WebSocket disconnected")

        except Exception as e:
            self.emit_log("error", f"Error disconnecting WebSocket: {e}")
            logger.error(f"WebSocket disconnect error: {traceback.format_exc()}")

    def _try_reconnect(self) -> bool:
        """
        Attempt to reconnect to WebSocket server.

        Returns:
            True if reconnection successful, False otherwise
        """
        with self._reconnect_lock:
            if self._reconnect_in_progress:
                print(
                    "[info    ] WS_HANDLER: Reconnect already in progress, skipping",
                    file=sys.stderr,
                    flush=True,
                )
                return False

            self._reconnect_in_progress = True

        try:
            print(
                "[info    ] WS_HANDLER: Attempting automatic reconnection...",
                file=sys.stderr,
                flush=True,
            )
            self.emit_log("info", "Attempting automatic WebSocket reconnection...")

            # Clean up old state
            self.ws_enabled = False
            self.ws_client = None
            self.ws_loop = None
            self.ws_thread = None

            # Reconnect
            success = self.connect()

            if success:
                print(
                    "[info    ] WS_HANDLER: Automatic reconnection successful!",
                    file=sys.stderr,
                    flush=True,
                )
                self.emit_log("info", "WebSocket reconnection successful")
            else:
                print(
                    "[error   ] WS_HANDLER: Automatic reconnection failed",
                    file=sys.stderr,
                    flush=True,
                )
                self.emit_log("error", "WebSocket reconnection failed")

            return success

        finally:
            self._reconnect_in_progress = False

    def start_session(self, config_snapshot: dict[str, Any] | None = None) -> bool:
        """
        Start WebSocket session.

        If the connection is dead but was previously configured, this will
        attempt to automatically reconnect before starting the session.

        Args:
            config_snapshot: Optional configuration snapshot to send

        Returns:
            True if session started successfully, False otherwise
        """
        self.emit_log(
            "debug",
            f"[WS_START_SESSION] Entry: ws_enabled={self.ws_enabled}, "
            f"ws_client={self.ws_client is not None}, ws_loop={self.ws_loop is not None}, "
            f"ws_config={self.ws_config is not None}",
        )

        # Check if we need to reconnect
        loop_running = self._is_loop_running()
        self.emit_log("debug", f"[WS_START_SESSION] _is_loop_running={loop_running}")
        if self.ws_enabled and not loop_running:
            self.emit_log("debug", "[WS_START_SESSION] Marking disconnected (loop closed)")
            self._mark_disconnected("event loop closed")

        # If not connected but we have config, try to reconnect
        self.emit_log(
            "debug",
            f"[WS_START_SESSION] After check - ws_enabled={self.ws_enabled}, "
            f"ws_config.enabled={self.ws_config.enabled if self.ws_config else None}",
        )
        if not self.ws_enabled and self.ws_config and self.ws_config.enabled:
            self.emit_log("info", "[WS_START_SESSION] Connection dead, attempting auto-reconnect")
            reconnect_result = self._try_reconnect()
            self.emit_log("debug", f"[WS_START_SESSION] _try_reconnect returned {reconnect_result}")
            if not reconnect_result:
                self.emit_log("warning", "Cannot start WebSocket session: reconnection failed")
                return False

        self.emit_log(
            "debug",
            f"[WS_START_SESSION] Final check - ws_enabled={self.ws_enabled}, "
            f"ws_client={self.ws_client is not None}, ws_loop={self.ws_loop is not None}",
        )
        if not self.ws_enabled or not self.ws_client or not self.ws_loop:
            self.emit_log("warning", "Cannot start WebSocket session: not connected")
            return False

        try:
            self.emit_log("info", "Starting WebSocket session...")
            self.emit_log(
                "debug",
                f"[WS_START_SESSION] ws_client.is_connected={self.ws_client.is_connected}, "
                f"ws_client.ws={self.ws_client.ws is not None}",
            )

            future = asyncio.run_coroutine_threadsafe(
                self.ws_client.start_session(config_snapshot), self.ws_loop
            )
            success = future.result(timeout=10)

            if success:
                self.emit_log("info", f"WebSocket session started: {self.ws_client.session_id}")
                return True
            else:
                self.emit_log("error", "Failed to start WebSocket session")
                self.emit_log(
                    "debug",
                    f"[WS_START_SESSION] After failure: ws_client.is_connected={self.ws_client.is_connected}",
                )
                return False

        except RuntimeError as e:
            if "Event loop is closed" in str(e):
                self._mark_disconnected("event loop closed during session start")
                self.emit_log("error", "WebSocket event loop died during session start")
            else:
                self.emit_log("error", f"Error starting WebSocket session: {e}")
            logger.error(f"WebSocket session start error: {traceback.format_exc()}")
            return False
        except Exception as e:
            self.emit_log("error", f"Error starting WebSocket session: {e}")
            logger.error(f"WebSocket session start error: {traceback.format_exc()}")
            return False

    def end_session(self, status: str = "completed", error: str | None = None) -> bool:
        """
        End WebSocket session.

        Args:
            status: Session status (completed, failed, etc.)
            error: Optional error message if session failed

        Returns:
            True if session ended successfully, False otherwise
        """
        # Check event loop health first
        if not self._is_loop_running():
            if self.ws_enabled:
                self._mark_disconnected("event loop closed")
            # Session can't be ended if loop is dead, but don't treat this as failure
            # since the session effectively ended when the connection died
            self.emit_log("warning", "WebSocket session end skipped: connection already dead")
            return True  # Return True since session is effectively ended

        if not self.ws_enabled or not self.ws_client or not self.ws_loop:
            return False

        try:
            self.emit_log("info", f"Ending WebSocket session with status: {status}")

            future = asyncio.run_coroutine_threadsafe(
                self.ws_client.end_session(status, error), self.ws_loop
            )
            success = future.result(timeout=10)

            if success:
                self.emit_log("info", "WebSocket session ended")
            else:
                self.emit_log("error", "Failed to end WebSocket session")

            return success

        except RuntimeError as e:
            if "Event loop is closed" in str(e):
                self._mark_disconnected("event loop closed during session end")
                # Session is effectively ended since connection is dead
                return True
            self.emit_log("error", f"Error ending WebSocket session: {e}")
            logger.error(f"WebSocket session end error: {traceback.format_exc()}")
            return False
        except Exception as e:
            self.emit_log("error", f"Error ending WebSocket session: {e}")
            logger.error(f"WebSocket session end error: {traceback.format_exc()}")
            return False

    def send_log(self, level: str, message: str, log_data: dict[str, Any] | None = None):
        """
        Send log to WebSocket (non-blocking).

        Args:
            level: Log level (info, warning, error, debug)
            message: Log message
            log_data: Optional structured log data
        """
        if not self.ws_enabled or not self.ws_client or not self.ws_loop:
            return

        # Check event loop health
        if not self._is_loop_running():
            self._mark_disconnected("event loop closed")
            return

        try:
            # Schedule coroutine in WebSocket event loop
            asyncio.run_coroutine_threadsafe(
                self.ws_client.send_log(level, message, log_data), self.ws_loop
            )
        except RuntimeError as e:
            if "Event loop is closed" in str(e):
                self._mark_disconnected("event loop closed during send_log")
            else:
                logger.error(f"Error sending log to WebSocket: {e}")
        except Exception as e:
            logger.error(f"Error sending log to WebSocket: {e}")

    def send_screenshot(self, image, name: str, metadata: dict[str, Any] | None = None):
        """
        Send screenshot to WebSocket (non-blocking).

        Args:
            image: Base64 encoded image data
            name: Screenshot name/identifier
            metadata: Optional metadata about the screenshot
        """
        if not self.ws_enabled or not self.ws_client or not self.ws_loop:
            return

        # Check event loop health
        if not self._is_loop_running():
            self._mark_disconnected("event loop closed")
            return

        try:
            # Schedule coroutine in WebSocket event loop
            asyncio.run_coroutine_threadsafe(
                self.ws_client.send_screenshot(image, name, metadata), self.ws_loop
            )
        except RuntimeError as e:
            if "Event loop is closed" in str(e):
                self._mark_disconnected("event loop closed during send_screenshot")
            else:
                logger.error(f"Error sending screenshot to WebSocket: {e}")
        except Exception as e:
            logger.error(f"Error sending screenshot to WebSocket: {e}")

    def is_enabled(self) -> bool:
        """
        Check if WebSocket is enabled and the connection is healthy.

        This method verifies both that WebSocket was enabled AND that the
        underlying event loop is still running AND that the actual WebSocket
        client is connected. If any of these conditions fail, this will
        detect it and mark the connection as disconnected.

        Returns:
            True if WebSocket is enabled and healthy, False otherwise
        """
        if not self.ws_enabled:
            return False

        # Check if event loop is still alive
        if not self._is_loop_running():
            self._mark_disconnected("event loop closed")
            return False

        # Check if the actual WebSocket client is still connected
        if self.ws_client and not self.ws_client.is_connected:
            self._mark_disconnected("websocket client disconnected")
            return False

        return True

    @property
    def is_connected(self) -> bool:
        """
        Check if WebSocket is connected and healthy.

        Returns:
            True if WebSocket is connected and event loop is running, False otherwise
        """
        if not self.ws_enabled or not self.ws_client:
            return False

        # Check event loop health
        if not self._is_loop_running():
            self._mark_disconnected("event loop closed")
            return False

        return self.ws_client.is_connected

    def send_message(self, message: str) -> bool:
        """
        Send a generic message to the WebSocket server.

        This is used for sending arbitrary message types like extraction events.

        Args:
            message: JSON-encoded message string

        Returns:
            True if message sent successfully, False otherwise
        """
        if not self.ws_enabled or not self.ws_client or not self.ws_loop:
            return False

        # Check event loop health
        if not self._is_loop_running():
            self._mark_disconnected("event loop closed")
            return False

        try:
            # Parse the message to ensure it's valid JSON
            import json

            parsed = json.loads(message)

            # Schedule coroutine in WebSocket event loop
            asyncio.run_coroutine_threadsafe(self.ws_client.send_message(parsed), self.ws_loop)
            return True
        except RuntimeError as e:
            if "Event loop is closed" in str(e):
                self._mark_disconnected("event loop closed during send_message")
            else:
                logger.error(f"Error sending message to WebSocket: {e}")
            return False
        except Exception as e:
            logger.error(f"Error sending message to WebSocket: {e}")
            return False

    def _handle_command(self, message: dict[str, Any]) -> None:
        """
        Handle incoming command from backend.

        This is called from the WebSocket client's message listener
        when a command message is received.

        Args:
            message: Command message with format:
                     {"type": "command", "command": "...", "params": {...}}
        """
        # Don't log high-frequency commands to avoid flooding
        cmd = message.get("command") if isinstance(message, dict) else None
        if cmd not in ("ping", "status"):
            print(
                f"[info    ] WS_HANDLER: Received command: {message}", file=sys.stderr, flush=True
            )

        if self.on_command_fn:
            try:
                # Extract command details and forward to the bridge
                self.on_command_fn(message)
            except Exception as e:
                print(
                    f"[error   ] WS_HANDLER: Error handling command: {e}",
                    file=sys.stderr,
                    flush=True,
                )
                import traceback

                print(
                    f"[error   ] WS_HANDLER: Traceback: {traceback.format_exc()}",
                    file=sys.stderr,
                    flush=True,
                )
        else:
            print(
                "[warning ] WS_HANDLER: No command handler registered", file=sys.stderr, flush=True
            )
