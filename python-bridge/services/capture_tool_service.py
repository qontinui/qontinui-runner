"""Screenshot Capture Tool Service for Configuration Builder.

This service captures screenshots on clicks for use in the qontinui-web configuration builder.
It is separate from the execution ScreenshotService and focuses specifically on
capturing training data for pattern matching.

This module provides two types of click capture:
1. Automation clicks: Captures screenshots when qontinui performs clicks during automation
2. Manual clicks: Captures screenshots when the user physically clicks their mouse
"""

import json
import re
import sys
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Dict, List, Literal, Optional

from PIL import Image


# Module-level flag to track if DPI awareness has been set
_dpi_awareness_set = False


def _set_windows_dpi_awareness_once() -> None:
    """Set Windows DPI awareness once per process.

    This must be called before any screen operations (MSS, pynput, etc.)
    to prevent DPI virtualization on high-DPI monitors.

    Requires Windows 10 1703+ for Per-Monitor DPI Awareness V2.
    """
    global _dpi_awareness_set

    import sys
    if sys.platform != "win32":
        return

    if _dpi_awareness_set:
        return

    try:
        import ctypes

        # Set Per-Monitor DPI Awareness V2 (Windows 10 1703+)
        shcore = ctypes.windll.shcore
        # PROCESS_PER_MONITOR_DPI_AWARE_V2 = 2
        result = shcore.SetProcessDpiAwareness(2)

        # S_OK = 0, E_ACCESSDENIED = -2147024891 (already set by another component)
        if result == 0 or result == -2147024891:
            _dpi_awareness_set = True
            print(f"[DPI] DPI awareness set successfully (result={result})", file=sys.stderr, flush=True)
        else:
            print(f"[DPI] DPI awareness unexpected result: {result}", file=sys.stderr, flush=True)
            _dpi_awareness_set = True  # Mark as attempted

    except Exception as e:
        # Don't let DPI awareness failures crash the process
        print(f"[DPI] Warning: Failed to set DPI awareness: {type(e).__name__}: {e}", file=sys.stderr, flush=True)
        _dpi_awareness_set = True  # Mark as attempted to avoid retry


@dataclass
class ScreenSelection:
    """Defines which screen(s) to capture."""

    type: Literal['all', 'primary', 'specific']
    indices: Optional[List[int]] = None

    def to_dict(self) -> Dict[str, Any]:
        """Convert to dictionary."""
        result = {"type": self.type}
        if self.indices is not None:
            result["indices"] = self.indices
        return result

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> 'ScreenSelection':
        """Create from dictionary."""
        return cls(
            type=data["type"],
            indices=data.get("indices")
        )


@dataclass
class ScreenshotCaptureSettings:
    """Configuration for screenshot capture tool."""

    enabled: bool
    manual_clicks_enabled: bool
    output_folder: str
    base_image_name: str
    screens: ScreenSelection
    capture_timings: List[int]  # delays in ms (0 = immediate)

    def to_dict(self) -> Dict[str, Any]:
        """Convert to dictionary with camelCase field names to match Rust."""
        return {
            "enabled": self.enabled,
            "manualClicksEnabled": self.manual_clicks_enabled,
            "outputFolder": self.output_folder,
            "baseImageName": self.base_image_name,
            "screens": self.screens.to_dict(),
            "captureTimings": self.capture_timings
        }

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> 'ScreenshotCaptureSettings':
        """Create from dictionary.

        Expects camelCase field names to match Rust serialization:
        - manualClicksEnabled
        - outputFolder
        - baseImageName
        - captureTimings
        """
        return cls(
            enabled=data["enabled"],
            manual_clicks_enabled=data.get("manualClicksEnabled", False),
            output_folder=data["outputFolder"],
            base_image_name=data["baseImageName"],
            screens=ScreenSelection.from_dict(data["screens"]),
            capture_timings=data["captureTimings"]
        )


class CaptureToolService:
    """Service for capturing screenshots on clicks for the configuration builder.

    This service:
    1. Captures screenshots using MSS (same as FIND operations)
    2. Supports multiple captures per click with configurable delays
    3. Auto-numbers screenshots based on existing files
    4. Supports screen selection (all, primary, or specific monitors)
    5. Saves screenshots to configured folder with base name

    Directory Structure:
        output_folder/
            {base_name}_1.png
            {base_name}_2.png
            {base_name}_3.png
            ...

    Each click can trigger multiple screenshots at different times:
    - Timing of 0ms = immediate screenshot at click time
    - Timing of 500ms = screenshot 500ms after click
    - etc.
    """

    def __init__(
        self,
        settings: Optional[ScreenshotCaptureSettings] = None,
        mss_capture: Optional[Any] = None
    ) -> None:
        """Initialize the CaptureToolService.

        Args:
            settings: Screenshot capture settings. If None, service is disabled.
            mss_capture: MSSScreenCapture instance from qontinui.hal. If None,
                        will be lazily initialized when needed.
        """
        self.settings = settings
        self.mss_capture = mss_capture
        self._capture_lock = threading.Lock()
        self._pending_captures: List[threading.Timer] = []

    def update_settings(self, settings: ScreenshotCaptureSettings) -> None:
        """Update capture settings.

        Args:
            settings: New screenshot capture settings
        """
        with self._capture_lock:
            self.settings = settings

    def is_enabled(self) -> bool:
        """Check if capture tool is enabled.

        Returns:
            True if enabled and configured
        """
        return self.settings is not None and self.settings.enabled

    def on_click(self, x: int, y: int, button: str = "left") -> None:
        """Handle click event and schedule screenshots.

        This is called when a click occurs during automation. It schedules
        screenshots according to the configured capture_timings.

        Args:
            x: Click x coordinate
            y: Click y coordinate
            button: Mouse button clicked
        """
        if not self.is_enabled():
            return

        # Initialize MSS if needed
        if self.mss_capture is None:
            self._initialize_mss()

        if self.mss_capture is None:
            # Failed to initialize
            return

        # Schedule screenshots at configured timings
        for delay_ms in self.settings.capture_timings:
            if delay_ms == 0:
                # Immediate capture
                self._capture_screenshot()
            else:
                # Delayed capture
                delay_seconds = delay_ms / 1000.0
                timer = threading.Timer(delay_seconds, self._capture_screenshot)
                timer.daemon = True
                timer.start()
                self._pending_captures.append(timer)

    def _initialize_mss(self) -> None:
        """Initialize MSS screen capture."""
        try:
            print("[CaptureService] Initializing MSS capture...", file=sys.stderr, flush=True)

            # CRITICAL: Set DPI awareness before creating MSS
            _set_windows_dpi_awareness_once()

            # Import qontinui HAL
            from qontinui.hal.implementations.mss_capture import MSSScreenCapture
            from qontinui.hal.config import HALConfig

            print("[CaptureService] Imports successful, creating HALConfig...", file=sys.stderr, flush=True)

            # Create MSS instance
            config = HALConfig()
            config.capture_cache_enabled = False  # Don't cache for capture tool

            print("[CaptureService] Creating MSSScreenCapture instance...", file=sys.stderr, flush=True)
            self.mss_capture = MSSScreenCapture(config)

            print(f"[CaptureService] ✓ MSS initialized successfully, found {len(self.mss_capture.get_monitors())} monitors", file=sys.stderr, flush=True)

        except ImportError as e:
            print(f"[CaptureService] ✗ Failed to import qontinui MSS capture: {e}", file=sys.stderr, flush=True)
            import traceback
            traceback.print_exc(file=sys.stderr)
            self.mss_capture = None
        except Exception as e:
            print(f"[CaptureService] ✗ Failed to initialize MSS capture: {type(e).__name__}: {e}", file=sys.stderr, flush=True)
            import traceback
            traceback.print_exc(file=sys.stderr)
            self.mss_capture = None

    def _capture_screenshot(self) -> None:
        """Capture and save screenshot based on settings."""
        if not self.is_enabled() or self.mss_capture is None:
            return

        try:
            with self._capture_lock:
                # Determine which screen(s) to capture
                images = self._capture_screens()

                # Save each image
                for image in images:
                    self._save_screenshot(image)

        except Exception as e:
            print(f"Error capturing screenshot: {e}", file=sys.stderr, flush=True)

    def _capture_screens(self) -> List[Image.Image]:
        """Capture screen(s) based on settings.

        Returns:
            List of PIL Images (one per screen captured)
        """
        images = []

        if self.settings.screens.type == 'all':
            # Capture all monitors
            monitors = self.mss_capture.get_monitors()
            for monitor in monitors:
                print(f"[CaptureService] Capturing monitor {monitor.index}: expected {monitor.width}x{monitor.height}", file=sys.stderr, flush=True)
                image = self.mss_capture.capture_screen(monitor.index)
                print(f"[CaptureService] Captured monitor {monitor.index}: actual {image.width}x{image.height}", file=sys.stderr, flush=True)
                images.append(image)

        elif self.settings.screens.type == 'primary':
            # Capture primary monitor only
            image = self.mss_capture.capture_screen(None)
            print(f"[CaptureService] Captured primary monitor: {image.width}x{image.height}", file=sys.stderr, flush=True)
            images.append(image)

        elif self.settings.screens.type == 'specific':
            # Capture specific monitors
            if self.settings.screens.indices:
                for monitor_index in self.settings.screens.indices:
                    try:
                        monitors = self.mss_capture.get_monitors()
                        monitor = monitors[monitor_index] if monitor_index < len(monitors) else None
                        if monitor:
                            print(f"[CaptureService] Capturing monitor {monitor_index}: expected {monitor.width}x{monitor.height}", file=sys.stderr, flush=True)
                        image = self.mss_capture.capture_screen(monitor_index)
                        print(f"[CaptureService] Captured monitor {monitor_index}: actual {image.width}x{image.height}", file=sys.stderr, flush=True)
                        images.append(image)
                    except Exception as e:
                        print(f"Failed to capture monitor {monitor_index}: {e}", file=sys.stderr, flush=True)

        return images

    def _save_screenshot(self, image: Image.Image) -> None:
        """Save screenshot with auto-numbering.

        Args:
            image: PIL Image to save
        """
        # Get next available number
        next_num = self._get_next_number()

        # Create output directory if needed
        output_dir = Path(self.settings.output_folder)
        output_dir.mkdir(parents=True, exist_ok=True)

        # Create filename
        filename = f"{self.settings.base_image_name}_{next_num}.png"
        filepath = output_dir / filename

        # Save image
        image.save(filepath, format='PNG')

        print(f"Screenshot saved: {filepath}", file=sys.stderr, flush=True)

    def _get_next_number(self) -> int:
        """Get next available number for screenshot filename.

        Finds all existing files with the base name and returns the
        largest number + 1, or 1 if no files exist.

        Returns:
            Next available number (1-based)
        """
        output_dir = Path(self.settings.output_folder)

        if not output_dir.exists():
            return 1

        # Find all files matching base name pattern
        pattern = f"{self.settings.base_image_name}_*.png"
        existing_files = list(output_dir.glob(pattern))

        if not existing_files:
            return 1

        # Extract numbers from filenames
        numbers = []
        for file in existing_files:
            # Match pattern: {base_name}_{number}.png
            match = re.match(
                rf"{re.escape(self.settings.base_image_name)}_(\d+)\.png",
                file.name
            )
            if match:
                numbers.append(int(match.group(1)))

        if not numbers:
            return 1

        return max(numbers) + 1

    def cancel_pending_captures(self) -> None:
        """Cancel all pending delayed captures.

        Call this when stopping execution or changing settings.
        """
        with self._capture_lock:
            for timer in self._pending_captures:
                timer.cancel()
            self._pending_captures.clear()

    def cleanup(self) -> None:
        """Clean up resources."""
        self.cancel_pending_captures()

        if self.mss_capture is not None:
            try:
                self.mss_capture.close()
            except Exception:
                pass
            self.mss_capture = None


class ManualClickListener:
    """Listens for manual mouse clicks and triggers screenshot capture.

    This class uses pynput to listen for physical mouse clicks on the system
    and integrates with CaptureToolService to capture screenshots. It filters
    clicks by screen bounds to only capture clicks on configured screen(s).

    The listener runs in a background thread and can be started/stopped
    independently of automation execution.
    """

    def __init__(self, capture_service: CaptureToolService) -> None:
        """Initialize the manual click listener.

        Args:
            capture_service: CaptureToolService instance to use for capturing
        """
        self.capture_service = capture_service
        self._listener = None
        self._listener_thread = None
        self._is_running = False
        self._lock = threading.Lock()

    def _get_screen_bounds(self) -> List[Dict[str, int]]:
        """Get bounds of screens to monitor for clicks.

        Returns:
            List of dicts with keys: x, y, width, height
        """
        if not self.capture_service.is_enabled():
            print(f"[ManualClickListener] Capture service not enabled", file=sys.stderr, flush=True)
            return []

        settings = self.capture_service.settings
        bounds = []

        # Initialize MSS if needed to get monitor info
        if self.capture_service.mss_capture is None:
            print(f"[ManualClickListener] Initializing MSS capture", file=sys.stderr, flush=True)
            self.capture_service._initialize_mss()

        if self.capture_service.mss_capture is None:
            print(f"[ManualClickListener] Failed to initialize MSS capture", file=sys.stderr, flush=True)
            return []

        try:
            monitors = self.capture_service.mss_capture.get_monitors()
            print(f"[ManualClickListener] Found {len(monitors)} monitors", file=sys.stderr, flush=True)

            if settings.screens.type == 'all':
                # All monitors
                print(f"[ManualClickListener] Screen selection: ALL", file=sys.stderr, flush=True)
                for monitor in monitors:
                    bounds.append({
                        'x': monitor.x,
                        'y': monitor.y,
                        'width': monitor.width,
                        'height': monitor.height
                    })
                    print(f"[ManualClickListener]   Monitor {monitor.index}: x={monitor.x}, y={monitor.y}, w={monitor.width}, h={monitor.height}, primary={monitor.is_primary}", file=sys.stderr, flush=True)
            elif settings.screens.type == 'primary':
                # Primary monitor only
                print(f"[ManualClickListener] Screen selection: PRIMARY", file=sys.stderr, flush=True)
                for monitor in monitors:
                    print(f"[ManualClickListener]   Checking monitor {monitor.index}: primary={monitor.is_primary}", file=sys.stderr, flush=True)
                    if monitor.is_primary:
                        bounds.append({
                            'x': monitor.x,
                            'y': monitor.y,
                            'width': monitor.width,
                            'height': monitor.height
                        })
                        print(f"[ManualClickListener]   Primary monitor found: x={monitor.x}, y={monitor.y}, w={monitor.width}, h={monitor.height}", file=sys.stderr, flush=True)
                        break
            elif settings.screens.type == 'specific' and settings.screens.indices:
                # Specific monitors
                print(f"[ManualClickListener] Screen selection: SPECIFIC {settings.screens.indices}", file=sys.stderr, flush=True)
                for monitor in monitors:
                    if monitor.index in settings.screens.indices:
                        bounds.append({
                            'x': monitor.x,
                            'y': monitor.y,
                            'width': monitor.width,
                            'height': monitor.height
                        })
                        print(f"[ManualClickListener]   Monitor {monitor.index}: x={monitor.x}, y={monitor.y}, w={monitor.width}, h={monitor.height}", file=sys.stderr, flush=True)
        except Exception as e:
            print(f"[ManualClickListener] Error getting screen bounds: {e}", file=sys.stderr, flush=True)
            import traceback
            traceback.print_exc()

        print(f"[ManualClickListener] Total screen bounds configured: {len(bounds)}", file=sys.stderr, flush=True)
        return bounds

    def _is_click_in_bounds(self, x: int, y: int) -> bool:
        """Check if click coordinates are within configured screen bounds.

        Args:
            x: Click x coordinate
            y: Click y coordinate

        Returns:
            True if click is within configured screens
        """
        bounds = self._get_screen_bounds()

        if not bounds:
            # No bounds configured, don't capture
            return False

        for screen in bounds:
            # Check if click is within this screen's bounds
            if (screen['x'] <= x < screen['x'] + screen['width'] and
                screen['y'] <= y < screen['y'] + screen['height']):
                return True

        return False

    def _on_click(self, x: int, y: int, button, pressed: bool) -> None:
        """Handle mouse click event from pynput.

        Args:
            x: Click x coordinate
            y: Click y coordinate
            button: Mouse button
            pressed: True if button pressed, False if released
        """
        # Only capture on button press (not release)
        if not pressed:
            return

        # Only capture left clicks by default
        try:
            from pynput.mouse import Button
            if button != Button.left:
                print(f"[ManualClickListener] Non-left button click ignored at ({x}, {y})", file=sys.stderr, flush=True)
                return
        except Exception:
            # If we can't check button type, proceed anyway
            pass

        print(f"[ManualClickListener] Left click detected at ({x}, {y})", file=sys.stderr, flush=True)

        # Check if capture is enabled
        if not self.capture_service.is_enabled():
            print(f"[ManualClickListener] Capture service not enabled, ignoring click", file=sys.stderr, flush=True)
            return

        # Check if click is within configured screen bounds
        bounds = self._get_screen_bounds()
        print(f"[ManualClickListener] Checking click against {len(bounds)} screen bounds", file=sys.stderr, flush=True)

        if not self._is_click_in_bounds(x, y):
            print(f"[ManualClickListener] Click at ({x}, {y}) is outside configured screen bounds, ignoring", file=sys.stderr, flush=True)
            return

        # Trigger screenshot capture
        print(f"[ManualClickListener] Click at ({x}, {y}) is within bounds, capturing screenshot...", file=sys.stderr, flush=True)
        self.capture_service.on_click(x, y, "left")

    def start(self) -> bool:
        """Start listening for manual mouse clicks.

        Returns:
            True if started successfully, False otherwise
        """
        print(f"[ManualClickListener] ========== start() called ==========", file=sys.stderr, flush=True)

        # CRITICAL: Set DPI awareness before starting mouse listener
        # This ensures pynput receives correct coordinates on high-DPI monitors
        _set_windows_dpi_awareness_once()

        # First check if pynput is available
        try:
            import pynput
            print(f"[ManualClickListener] pynput is installed (version: {pynput.__version__ if hasattr(pynput, '__version__') else 'unknown'})", file=sys.stderr, flush=True)
        except ImportError as e:
            print(f"[ManualClickListener] ERROR: pynput is NOT installed!", file=sys.stderr, flush=True)
            print(f"[ManualClickListener]   ImportError: {e}", file=sys.stderr, flush=True)
            print(f"[ManualClickListener]   Install it with: pip install pynput", file=sys.stderr, flush=True)
            return False

        with self._lock:
            if self._is_running:
                print("[ManualClickListener] Already running", file=sys.stderr, flush=True)
                return False

            if not self.capture_service.is_enabled():
                print("[ManualClickListener] Cannot start: capture service not enabled", file=sys.stderr, flush=True)
                print(f"[ManualClickListener]   settings: {self.capture_service.settings}", file=sys.stderr, flush=True)
                return False

            try:
                from pynput import mouse

                # Create and start listener
                print(f"[ManualClickListener] Creating mouse.Listener...", file=sys.stderr, flush=True)
                self._listener = mouse.Listener(on_click=self._on_click)
                print(f"[ManualClickListener] Calling listener.start()...", file=sys.stderr, flush=True)
                self._listener.start()
                self._is_running = True

                print("[ManualClickListener] ✓✓✓ Started successfully and listening for clicks ✓✓✓", file=sys.stderr, flush=True)
                print(f"[ManualClickListener]   Capture settings: enabled={self.capture_service.settings.enabled}", file=sys.stderr, flush=True)
                print(f"[ManualClickListener]   Screen selection: {self.capture_service.settings.screens.to_dict()}", file=sys.stderr, flush=True)
                print(f"[ManualClickListener]   Output folder: {self.capture_service.settings.output_folder}", file=sys.stderr, flush=True)
                print(f"[ManualClickListener] ===============================================", file=sys.stderr, flush=True)
                return True

            except ImportError as e:
                print(f"[ManualClickListener] ERROR: Failed to import pynput.mouse: {e}", file=sys.stderr, flush=True)
                return False
            except Exception as e:
                print(f"[ManualClickListener] ERROR: Failed to start: {e}", file=sys.stderr, flush=True)
                import traceback
                traceback.print_exc()
                return False

    def stop(self) -> None:
        """Stop listening for manual mouse clicks."""
        with self._lock:
            if not self._is_running:
                return

            if self._listener is not None:
                try:
                    self._listener.stop()
                except Exception as e:
                    print(f"Error stopping listener: {e}", file=sys.stderr, flush=True)
                finally:
                    self._listener = None

            self._is_running = False
            print("Manual click listener stopped", file=sys.stderr, flush=True)

    def is_running(self) -> bool:
        """Check if listener is currently running.

        Returns:
            True if running, False otherwise
        """
        return self._is_running

    def cleanup(self) -> None:
        """Clean up resources."""
        self.stop()
