"""Click-to-template capture commands.

A mixin of ``QontinuiExecutor``. Its methods were moved verbatim from
``qontinui_executor.py`` by plan
2026-10-04-runner-python-executor-routes-118-commands-through-one-if-chain
and still read executor state through ``self``. ``COMMANDS`` maps each command
name to its handler method.
"""

import sys
import time
from typing import Any, ClassVar

from ._host import ExecutorHost
from ._shared import QONTINUI_AVAILABLE, RUNNER_PARENT_DIR, InputMonitorService


class ClickCaptureCommands(ExecutorHost):
    """Click-to-template capture commands."""

    COMMANDS: ClassVar[dict[str, str]] = {
        # Click-to-Template capture commands
        "start_click_capture": "_handle_start_click_capture",
        "stop_click_capture": "_handle_stop_click_capture",
        "get_click_capture_status": "_handle_get_click_capture_status",
        "process_click_capture": "_handle_process_click_capture",
    }

    # =========================================================================
    # Click-to-Template Capture Commands
    # =========================================================================

    def _handle_start_click_capture(self, params: dict[str, Any]) -> dict[str, Any]:
        """Start click capture for template extraction.

        This captures input events (clicks) that will later be processed
        to extract template candidates using the qontinui library.

        Args:
            params: Command parameters:
                - session_id: Optional session ID (auto-generated if not provided)
                - output_dir: Optional output directory
                - application_hint: Optional application name for profile lookup

        Returns:
            Dictionary with:
                - success: Whether capture started
                - session_id: The session ID being used
                - output_dir: Directory where files will be saved
                - error: Error message (if failed)
        """
        import uuid
        from pathlib import Path

        if self._click_capture_active:
            return {
                "success": False,
                "error": "Click capture already active",
                "session_id": self._click_capture_session_id,
            }

        try:
            # Generate session ID if not provided
            session_id = params.get("session_id") or f"click-capture-{uuid.uuid4().hex[:8]}"
            output_dir = params.get("output_dir")
            application_hint = params.get("application_hint")

            # Set up output directory
            if output_dir:
                capture_dir = Path(output_dir)
            else:
                dev_logs_dir = RUNNER_PARENT_DIR / ".dev-logs"
                capture_dir = dev_logs_dir / "click-captures"

            capture_dir.mkdir(parents=True, exist_ok=True)

            # Initialize InputMonitorService if not already done
            if self.input_monitor_service is None:
                self.input_monitor_service = InputMonitorService(storage_dir=capture_dir)
                self.event_manager.emit_log(
                    "info",
                    f"InputMonitorService initialized for click capture: {capture_dir}",
                )

            # Start input monitoring (captures clicks)
            self.input_monitor_service.start_monitoring(session_id=session_id, fps=30)
            self.event_manager.emit_log("info", f"Click capture started for session: {session_id}")

            # Update state
            self._click_capture_active = True
            self._click_capture_session_id = session_id
            self._click_capture_start_time = time.time()
            self._click_capture_output_dir = str(capture_dir)
            self._click_capture_application_hint = application_hint

            # Emit capture started event
            self.event_manager.emit_event_wrapper(
                "click_capture_started",
                {
                    "session_id": session_id,
                    "output_dir": str(capture_dir),
                    "application_hint": application_hint,
                },
            )

            return {
                "success": True,
                "session_id": session_id,
                "output_dir": str(capture_dir),
                "application_hint": application_hint,
            }

        except Exception as e:
            self.event_manager.emit_log("error", f"Failed to start click capture: {e}")
            import traceback

            traceback.print_exc(file=sys.stderr)
            return {"success": False, "error": str(e)}

    def _handle_stop_click_capture(self, params: dict[str, Any]) -> dict[str, Any]:
        """Stop click capture and optionally process results.

        Args:
            params: Command parameters:
                - process_immediately: Whether to process captures now (default: False)

        Returns:
            Dictionary with:
                - success: Whether capture stopped successfully
                - session_id: The session ID
                - events_file: Path to the input events JSONL file
                - events_count: Number of click events captured
                - duration: Capture duration in seconds
                - candidates: Template candidates (if process_immediately=True)
                - error: Error message (if failed)
        """
        if not self._click_capture_active:
            return {
                "success": False,
                "error": "No click capture active",
            }

        try:
            session_id = self._click_capture_session_id
            start_time = self._click_capture_start_time
            output_dir = self._click_capture_output_dir
            application_hint = self._click_capture_application_hint
            process_immediately = params.get("process_immediately", False)

            # Stop input monitoring
            events_file = None
            events_count = 0
            if self.input_monitor_service:
                events_file = self.input_monitor_service.stop_monitoring()
                # Filter for click events only
                all_events = self.input_monitor_service.get_events()
                click_events = [e for e in all_events if e.get("event_type") == "mouse_click"]
                events_count = len(click_events)

            # Calculate duration
            duration = time.time() - start_time if start_time else 0

            # Reset state
            self._click_capture_active = False
            self._click_capture_session_id = None
            self._click_capture_start_time = None
            self._click_capture_output_dir = None
            self._click_capture_application_hint = None

            self.event_manager.emit_log(
                "info",
                f"Click capture stopped: {events_count} click events captured, "
                f"duration={duration:.1f}s, file={events_file}",
            )

            result = {
                "success": True,
                "session_id": session_id,
                "events_file": events_file,
                "events_count": events_count,
                "duration": duration,
                "output_dir": output_dir,
            }

            # Process immediately if requested
            if process_immediately and events_file:
                process_result = self._process_click_capture_internal(
                    events_file=events_file,
                    session_id=session_id,
                    application_hint=application_hint,
                )
                result["candidates"] = process_result.get("candidates", [])
                result["candidates_count"] = process_result.get("candidates_count", 0)

            # Emit capture stopped event
            self.event_manager.emit_event_wrapper(
                "click_capture_stopped",
                {
                    "session_id": session_id,
                    "events_file": events_file,
                    "events_count": events_count,
                    "duration": duration,
                },
            )

            return result

        except Exception as e:
            self.event_manager.emit_log("error", f"Failed to stop click capture: {e}")
            import traceback

            traceback.print_exc(file=sys.stderr)
            # Reset state even on error
            self._click_capture_active = False
            return {"success": False, "error": str(e)}

    def _handle_get_click_capture_status(self) -> dict[str, Any]:
        """Get current click capture status.

        Returns:
            Dictionary with:
                - success: Always True
                - is_capturing: Whether capture is active
                - session_id: Current session ID (if capturing)
                - duration: Current capture duration in seconds (if capturing)
                - events_count: Number of click events captured so far (if capturing)
        """
        if not self._click_capture_active:
            return {
                "success": True,
                "is_capturing": False,
            }

        duration = (
            time.time() - self._click_capture_start_time if self._click_capture_start_time else 0
        )
        events_count = 0
        if self.input_monitor_service:
            all_events = self.input_monitor_service.get_events()
            click_events = [e for e in all_events if e.get("event_type") == "mouse_click"]
            events_count = len(click_events)

        return {
            "success": True,
            "is_capturing": True,
            "session_id": self._click_capture_session_id,
            "duration": duration,
            "events_count": events_count,
            "output_dir": self._click_capture_output_dir,
            "application_hint": self._click_capture_application_hint,
        }

    def _handle_process_click_capture(self, params: dict[str, Any]) -> dict[str, Any]:
        """Process a completed click capture session to extract template candidates.

        Uses the qontinui library's CaptureProcessor to analyze screenshots
        at click timestamps and detect element boundaries.

        Args:
            params: Command parameters:
                - events_file: Path to the events JSONL file (required)
                - video_path: Path to the video file (optional - uses screenshots if not provided)
                - session_id: Session ID for the candidates
                - application_hint: Optional application name for profile lookup
                - send_to_web: Whether to send candidates to qontinui-web API (default: False)
                - web_api_url: URL for qontinui-web API (required if send_to_web=True)

        Returns:
            Dictionary with:
                - success: Whether processing succeeded
                - candidates_count: Number of template candidates extracted
                - candidates: List of candidate dictionaries
                - error: Error message (if failed)
        """
        events_file = params.get("events_file")
        if not events_file:
            return {"success": False, "error": "events_file is required"}

        return self._process_click_capture_internal(
            events_file=events_file,
            video_path=params.get("video_path"),
            session_id=params.get("session_id"),
            application_hint=params.get("application_hint"),
            send_to_web=params.get("send_to_web", False),
            web_api_url=params.get("web_api_url"),
        )

    def _process_click_capture_internal(
        self,
        events_file: str,
        video_path: str | None = None,
        session_id: str | None = None,
        application_hint: str | None = None,
        send_to_web: bool = False,
        web_api_url: str | None = None,
    ) -> dict[str, Any]:
        """Internal method to process click capture.

        If video_path is provided, extracts frames from video at click timestamps.
        Otherwise, captures screenshots at the current time for each click location.
        """
        import json
        import uuid
        from pathlib import Path

        try:
            # Import qontinui library modules
            from qontinui.discovery.click_analysis import (
                CaptureProcessor,
                ClickTemplateCandidate,
            )

            events_path = Path(events_file)
            if not events_path.exists():
                return {
                    "success": False,
                    "error": f"Events file not found: {events_file}",
                }

            # Load click events from JSONL
            click_events = []
            with open(events_path) as f:
                for line in f:
                    line = line.strip()
                    if not line:
                        continue
                    try:
                        event = json.loads(line)
                        if event.get("event_type") == "mouse_click":
                            click_events.append(event)
                    except json.JSONDecodeError:
                        continue

            if not click_events:
                return {
                    "success": True,
                    "candidates_count": 0,
                    "candidates": [],
                    "message": "No click events found in file",
                }

            self.event_manager.emit_log(
                "info",
                f"Processing {len(click_events)} click events from {events_file}",
            )

            processor = CaptureProcessor()
            candidates: list[ClickTemplateCandidate] = []

            if video_path and Path(video_path).exists():
                # Process from video file
                candidates = processor.process_capture_session(
                    video_path=Path(video_path),
                    events_file=events_path,
                    session_id=session_id or str(uuid.uuid4()),
                    application_hint=application_hint,
                )
            else:
                # Process from current screenshots (for each click location)
                # Take a screenshot now and process all clicks against it
                screenshot = self._capture_screenshot_for_processing()
                if screenshot is not None:
                    click_locations = [(e.get("x", 0), e.get("y", 0)) for e in click_events]
                    candidates = processor.process_screenshot_with_clicks(
                        screenshot=screenshot,
                        click_locations=click_locations,
                        session_id=session_id or str(uuid.uuid4()),
                        application_hint=application_hint,
                    )

            # Convert candidates to dictionaries
            candidate_dicts = [c.to_dict() for c in candidates]

            self.event_manager.emit_log("info", f"Extracted {len(candidates)} template candidates")

            result = {
                "success": True,
                "candidates_count": len(candidates),
                "candidates": candidate_dicts,
            }

            # Send to web API if requested
            if send_to_web and web_api_url and candidates:
                send_result = self._send_candidates_to_web(candidate_dicts, web_api_url)
                result["web_send_success"] = send_result.get("success", False)
                if not send_result.get("success"):
                    result["web_send_error"] = send_result.get("error")

            return result

        except ImportError as e:
            self.event_manager.emit_log("error", f"Failed to import qontinui library: {e}")
            return {"success": False, "error": f"qontinui library not available: {e}"}
        except Exception as e:
            self.event_manager.emit_log("error", f"Failed to process click capture: {e}")
            import traceback

            traceback.print_exc(file=sys.stderr)
            return {"success": False, "error": str(e)}

    def _capture_screenshot_for_processing(self):
        """Capture a screenshot for click capture processing."""
        try:
            if QONTINUI_AVAILABLE:
                from qontinui.hal import create_default_hal

                hal = create_default_hal()
                screenshot = hal.capture_screen()
                return screenshot
        except Exception as e:
            self.event_manager.emit_log(
                "warning", f"Failed to capture screenshot for processing: {e}"
            )
        return None

    def _send_candidates_to_web(
        self, candidates: list[dict[str, Any]], web_api_url: str
    ) -> dict[str, Any]:
        """Send template candidates to qontinui-web API."""
        import requests

        try:
            response = requests.post(
                f"{web_api_url}/api/v1/template-capture/candidates",
                json=candidates,
                timeout=30,
            )
            response.raise_for_status()
            return {"success": True}
        except Exception as e:
            self.event_manager.emit_log("error", f"Failed to send candidates to web API: {e}")
            return {"success": False, "error": str(e)}
