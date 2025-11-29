"""
Capture Manager Module

Single Responsibility: Handle screenshot and video capture
- Configure screenshot capture settings
- Manage manual click listener for capture tool
- Handle mouse click events for capture
- Coordinate with CaptureToolService
"""

import logging
from typing import Any, Callable, Dict

logger = logging.getLogger(__name__)

try:
    from services.capture_tool_service import (
        CaptureToolService,
        ScreenshotCaptureSettings,
        ManualClickListener,
    )
    from qontinui.reporting import register_callback, EventType as QontinuiEventType

    CAPTURE_AVAILABLE = True
except ImportError:
    CAPTURE_AVAILABLE = False
    logger.debug("Capture modules not available")


class CaptureManager:
    """
    Manages screenshot and video capture.

    Responsibilities:
    - Initialize capture tool service
    - Update capture settings
    - Start/stop manual click listener
    - Handle mouse click events
    """

    def __init__(
        self, emit_log_fn: Callable[[str, str], None], emit_event_fn: Callable[[Any, Dict], None]
    ):
        """
        Initialize capture manager.

        Args:
            emit_log_fn: Function to emit log messages (level, message)
            emit_event_fn: Function to emit events (event_type, data)
        """
        self.emit_log = emit_log_fn
        self.emit_event = emit_event_fn
        self.capture_tool_service = None
        self.manual_click_listener = None

        if CAPTURE_AVAILABLE:
            # Screenshot capture tool for configuration builder (qontinui-web)
            self.capture_tool_service = (
                CaptureToolService()
            )  # Initially disabled, enabled via update_settings

            # Manual click listener for capture tool (captures user clicks, not automation clicks)
            self.manual_click_listener = ManualClickListener(self.capture_tool_service)

            # Register callback for screenshot capture tool (MOUSE_CLICKED events)
            register_callback(QontinuiEventType.MOUSE_CLICKED, self._on_mouse_clicked)
        else:
            self.emit_log("debug", "Capture tools not available")

    def _on_mouse_clicked(self, event) -> None:
        """
        Handle MOUSE_CLICKED event for screenshot capture tool.

        Args:
            event: Mouse clicked event from qontinui library
        """
        if self.capture_tool_service and hasattr(event, "x") and hasattr(event, "y"):
            button = getattr(event, "button", "left")
            self.capture_tool_service.on_click(event.x, event.y, button)

    def update_settings(self, settings_dict: Dict[str, Any]) -> Dict[str, Any]:
        """
        Update screenshot capture tool settings.

        Automatically starts/stops the manual click listener based on settings.

        Args:
            settings_dict: Dictionary with capture settings:
                - enabled: bool - Enable/disable capture
                - manualClicksEnabled: bool - Enable/disable manual click capture
                - output_folder: str - Where to save screenshots
                - base_image_name: str - Base name for screenshot files
                - screens: dict - Screen selection configuration
                - capture_timings: list[int] - Capture delays in milliseconds

        Returns:
            Dict with success status
        """
        if not CAPTURE_AVAILABLE:
            return {"success": False, "error": "Capture tools not available"}

        self.emit_log(
            "info",
            f"update_capture_settings called: enabled={settings_dict.get('enabled')}, manualClicksEnabled={settings_dict.get('manualClicksEnabled')}",
        )

        try:
            settings = ScreenshotCaptureSettings.from_dict(settings_dict)
            self.emit_log(
                "info",
                f"Parsed settings: enabled={settings.enabled}, manual_clicks_enabled={settings.manual_clicks_enabled}",
            )
            self.emit_log("info", f"Output folder: {settings.output_folder}")

            self.capture_tool_service.update_settings(settings)

            # Start or stop manual click listener based on manual_clicks_enabled setting
            if settings.manual_clicks_enabled and settings.enabled:
                self.emit_log("info", "Attempting to start manual click listener")
                if not self.manual_click_listener.is_running():
                    self.emit_log("info", "Starting listener...")
                    success = self.manual_click_listener.start()
                    self.emit_log("info", f"Listener start result: {success}")
                    if success:
                        self.emit_log("info", "Manual click capture started successfully")
                    else:
                        self.emit_log("warning", "Failed to start manual click capture")
                else:
                    self.emit_log("info", "Listener already running")
            else:
                self.emit_log("info", f"Manual clicks disabled or capture disabled")
                if self.manual_click_listener.is_running():
                    self.emit_log("info", "Stopping listener")
                    self.manual_click_listener.stop()
                    self.emit_log("info", "Manual click capture stopped")

            is_running = self.manual_click_listener.is_running()
            self.emit_log("info", f"Final state: listener is_running={is_running}")

            # Emit event with manual capture status for frontend
            from event_manager import EventType

            self.emit_event(
                EventType.LOG,
                {
                    "level": "info",
                    "message": "capture_status_update",
                    "manual_capture_running": is_running,
                },
            )

            return {"success": True, "manual_capture_running": is_running}
        except Exception as e:
            self.emit_log("error", f"update_capture_settings failed: {e}")
            import traceback

            self.emit_log("error", f"Traceback: {traceback.format_exc()}")
            return {"success": False, "error": str(e)}

    def is_manual_capture_running(self) -> bool:
        """
        Check if manual click listener is running.

        Returns:
            True if manual click listener is running, False otherwise
        """
        if not CAPTURE_AVAILABLE or not self.manual_click_listener:
            return False
        return self.manual_click_listener.is_running()
