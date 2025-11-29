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
import threading
import time
import traceback
from typing import Any, Callable, Dict, Optional
import sys

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
        on_command_fn: Optional[Callable[[Dict[str, Any]], None]] = None,
    ):
        """
        Initialize WebSocket handler.

        Args:
            emit_log_fn: Function to emit log messages (level, message)
            on_command_fn: Optional callback for handling incoming commands from backend
        """
        self.emit_log = emit_log_fn
        self.on_command_fn = on_command_fn
        self.ws_client: Optional[RunnerWebSocketClient] = None
        self.ws_config: Optional[WebSocketConfig] = None
        self.ws_loop: Optional[asyncio.AbstractEventLoop] = None
        self.ws_thread: Optional[threading.Thread] = None
        self.ws_enabled = False

        # Initialize WebSocket configuration from environment
        if WEBSOCKET_AVAILABLE:
            try:
                self.ws_config = WebSocketConfig.from_env()
                if self.ws_config.enabled:
                    self.emit_log("info", "WebSocket configuration loaded from environment")
            except Exception as e:
                self.emit_log("error", f"Failed to load WebSocket config: {e}")

    def configure(
        self,
        enabled: bool,
        api_url: str,
        token: str,
        project_id: Optional[str] = None,
        runner_name: Optional[str] = None,
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

        print(f"[info    ] WS_HANDLER: connect() called", file=sys.stderr, flush=True)
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
                f"[error   ] WS_HANDLER: WebSocket libraries not available",
                file=sys.stderr,
                flush=True,
            )
            return False

        if not self.ws_config:
            print(
                f"[error   ] WS_HANDLER: WebSocket configuration not set",
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
            f"[info    ] WS_HANDLER: Config validation passed, starting connection...",
            file=sys.stderr,
            flush=True,
        )

        try:
            # Start event loop in background thread
            print(
                f"[info    ] WS_HANDLER: Starting event loop thread...", file=sys.stderr, flush=True
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
                        f"[error   ] WS_HANDLER: Event loop failed to start",
                        file=sys.stderr,
                        flush=True,
                    )
                    return False

            print(f"[info    ] WS_HANDLER: Event loop ready", file=sys.stderr, flush=True)

            # Get or refresh token
            token = self.ws_config.token
            print(
                f"[info    ] WS_HANDLER: token_len={len(token) if token else 0}",
                file=sys.stderr,
                flush=True,
            )
            if not token and self.ws_config.email and self.ws_config.password:
                print(
                    f"[info    ] WS_HANDLER: Authenticating with qontinui-web...",
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
                    self.ws_loop,
                )
                token = future.result(timeout=10)

                if not token:
                    print(
                        f"[error   ] WS_HANDLER: Authentication failed", file=sys.stderr, flush=True
                    )
                    return False

                self.ws_config.token = token
                print(
                    f"[info    ] WS_HANDLER: Authentication successful", file=sys.stderr, flush=True
                )

            # Create WebSocket client
            print(
                f"[info    ] WS_HANDLER: Creating WebSocket client...", file=sys.stderr, flush=True
            )
            print(
                f"[info    ] WS_HANDLER: api_url={self.ws_config.api_url}, project_id={self.ws_config.project_id}, runner_name={self.ws_config.runner_name}",
                file=sys.stderr,
                flush=True,
            )
            self.ws_client = RunnerWebSocketClient(
                api_url=self.ws_config.api_url,
                token=token,
                project_id=self.ws_config.project_id,
                runner_version=self.ws_config.runner_version,
                runner_name=self.ws_config.runner_name,
                auto_reconnect=self.ws_config.auto_reconnect,
                heartbeat_interval=self.ws_config.heartbeat_interval,
                max_reconnect_attempts=self.ws_config.max_reconnect_attempts,
                on_connected=lambda: print(
                    f"[info    ] WS_HANDLER: WebSocket connected callback",
                    file=sys.stderr,
                    flush=True,
                ),
                on_disconnected=lambda: print(
                    f"[warning ] WS_HANDLER: WebSocket disconnected callback",
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
                    f"[info    ] WS_HANDLER: Command handler registered",
                    file=sys.stderr,
                    flush=True,
                )

            # Connect
            print(
                f"[info    ] WS_HANDLER: Calling ws_client.connect()...",
                file=sys.stderr,
                flush=True,
            )
            future = asyncio.run_coroutine_threadsafe(self.ws_client.connect(), self.ws_loop)
            success = future.result(timeout=10)
            print(
                f"[info    ] WS_HANDLER: ws_client.connect() returned: {success}",
                file=sys.stderr,
                flush=True,
            )

            if success:
                self.ws_enabled = True
                print(
                    f"[info    ] WS_HANDLER: WebSocket connection established!",
                    file=sys.stderr,
                    flush=True,
                )
                return True
            else:
                print(
                    f"[error   ] WS_HANDLER: WebSocket connection failed",
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

    def start_session(self, config_snapshot: Optional[Dict[str, Any]] = None) -> bool:
        """
        Start WebSocket session.

        Args:
            config_snapshot: Optional configuration snapshot to send

        Returns:
            True if session started successfully, False otherwise
        """
        if not self.ws_enabled or not self.ws_client or not self.ws_loop:
            self.emit_log("warning", "Cannot start WebSocket session: not connected")
            return False

        try:
            self.emit_log("info", "Starting WebSocket session...")

            future = asyncio.run_coroutine_threadsafe(
                self.ws_client.start_session(config_snapshot), self.ws_loop
            )
            success = future.result(timeout=10)

            if success:
                self.emit_log("info", f"WebSocket session started: {self.ws_client.session_id}")
                return True
            else:
                self.emit_log("error", "Failed to start WebSocket session")
                return False

        except Exception as e:
            self.emit_log("error", f"Error starting WebSocket session: {e}")
            logger.error(f"WebSocket session start error: {traceback.format_exc()}")
            return False

    def end_session(self, status: str = "completed", error: Optional[str] = None) -> bool:
        """
        End WebSocket session.

        Args:
            status: Session status (completed, failed, etc.)
            error: Optional error message if session failed

        Returns:
            True if session ended successfully, False otherwise
        """
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

        except Exception as e:
            self.emit_log("error", f"Error ending WebSocket session: {e}")
            logger.error(f"WebSocket session end error: {traceback.format_exc()}")
            return False

    def send_log(self, level: str, message: str, log_data: Optional[Dict[str, Any]] = None):
        """
        Send log to WebSocket (non-blocking).

        Args:
            level: Log level (info, warning, error, debug)
            message: Log message
            log_data: Optional structured log data
        """
        if not self.ws_enabled or not self.ws_client or not self.ws_loop:
            return

        try:
            # Schedule coroutine in WebSocket event loop
            asyncio.run_coroutine_threadsafe(
                self.ws_client.send_log(level, message, log_data), self.ws_loop
            )
        except Exception as e:
            logger.error(f"Error sending log to WebSocket: {e}")

    def send_screenshot(self, image, name: str, metadata: Optional[Dict[str, Any]] = None):
        """
        Send screenshot to WebSocket (non-blocking).

        Args:
            image: Base64 encoded image data
            name: Screenshot name/identifier
            metadata: Optional metadata about the screenshot
        """
        if not self.ws_enabled or not self.ws_client or not self.ws_loop:
            return

        try:
            # Schedule coroutine in WebSocket event loop
            asyncio.run_coroutine_threadsafe(
                self.ws_client.send_screenshot(image, name, metadata), self.ws_loop
            )
        except Exception as e:
            logger.error(f"Error sending screenshot to WebSocket: {e}")

    def is_enabled(self) -> bool:
        """
        Check if WebSocket is enabled and connected.

        Returns:
            True if WebSocket is enabled, False otherwise
        """
        return self.ws_enabled

    @property
    def is_connected(self) -> bool:
        """
        Check if WebSocket is connected.

        Returns:
            True if WebSocket is connected, False otherwise
        """
        return self.ws_enabled and self.ws_client is not None and self.ws_client.is_connected

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

        try:
            # Parse the message to ensure it's valid JSON
            import json

            parsed = json.loads(message)

            # Schedule coroutine in WebSocket event loop
            asyncio.run_coroutine_threadsafe(self.ws_client.send_message(parsed), self.ws_loop)
            return True
        except Exception as e:
            logger.error(f"Error sending message to WebSocket: {e}")
            return False

    def _handle_command(self, message: Dict[str, Any]) -> None:
        """
        Handle incoming command from backend.

        This is called from the WebSocket client's message listener
        when a command message is received.

        Args:
            message: Command message with format:
                     {"type": "command", "command": "...", "params": {...}}
        """
        print(f"[info    ] WS_HANDLER: Received command: {message}", file=sys.stderr, flush=True)

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
                f"[warning ] WS_HANDLER: No command handler registered", file=sys.stderr, flush=True
            )
