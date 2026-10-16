"""Interaction recording commands.

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
from ._shared import RUNNER_PARENT_DIR, InputMonitorService


class RecordingCommands(ExecutorHost):
    """Interaction recording commands."""

    COMMANDS: ClassVar[dict[str, str]] = {
        # Interaction Recording commands (video + input capture for State Machine creation)
        "start_interaction_recording": "_handle_start_interaction_recording",
        "stop_interaction_recording": "_handle_stop_interaction_recording",
        "get_interaction_recording_status": "_handle_get_interaction_recording_status",
    }

    def _handle_start_interaction_recording(self, params: dict[str, Any]) -> dict[str, Any]:
        """Start interaction recording (video + input capture).

        This combines video recording and input monitoring for capturing user
        interactions to be used for State Machine creation.

        Args:
            params: Command parameters:
                - session_id: Optional session ID (auto-generated if not provided)
                - fps: Frames per second for video (default: 30)
                - output_dir: Optional output directory (defaults to .dev-logs/interactions)

        Returns:
            Dictionary with:
                - success: Whether recording started
                - session_id: The session ID being used
                - output_dir: Directory where files will be saved
                - error: Error message (if failed)
        """
        import uuid
        from pathlib import Path

        if self._interaction_recording_active:
            return {
                "success": False,
                "error": "Interaction recording already active",
                "session_id": self._interaction_session_id,
            }

        try:
            # Generate session ID if not provided
            session_id = params.get("session_id") or f"interaction-{uuid.uuid4().hex[:8]}"
            fps = params.get("fps", 30)
            output_dir = params.get("output_dir")

            # Set up output directory
            if output_dir:
                interactions_dir = Path(output_dir)
            else:
                dev_logs_dir = RUNNER_PARENT_DIR / ".dev-logs"
                interactions_dir = dev_logs_dir / "interactions"

            interactions_dir.mkdir(parents=True, exist_ok=True)

            # Initialize InputMonitorService if not already done
            if self.input_monitor_service is None:
                self.input_monitor_service = InputMonitorService(storage_dir=interactions_dir)
                self.event_manager.emit_log(
                    "info", f"InputMonitorService initialized: {interactions_dir}"
                )

            # Start input monitoring
            self.input_monitor_service.start_monitoring(session_id=session_id, fps=fps)
            self.event_manager.emit_log(
                "info", f"Input monitoring started for session: {session_id}"
            )

            # Update state
            self._interaction_recording_active = True
            self._interaction_session_id = session_id
            self._interaction_start_time = time.time()
            self._interaction_fps = fps

            # Emit recording started event (use emit_event_wrapper for string event types)
            self.event_manager.emit_event_wrapper(
                "interaction_recording_started",
                {
                    "session_id": session_id,
                    "fps": fps,
                    "output_dir": str(interactions_dir),
                },
            )

            return {
                "success": True,
                "session_id": session_id,
                "output_dir": str(interactions_dir),
                "fps": fps,
            }

        except Exception as e:
            self.event_manager.emit_log("error", f"Failed to start interaction recording: {e}")
            import traceback

            traceback.print_exc(file=sys.stderr)
            return {"success": False, "error": str(e)}

    def _handle_stop_interaction_recording(self) -> dict[str, Any]:
        """Stop interaction recording and return file paths.

        Returns:
            Dictionary with:
                - success: Whether recording stopped successfully
                - session_id: The session ID
                - events_file: Path to the input events JSONL file
                - events_count: Number of input events captured
                - duration: Recording duration in seconds
                - error: Error message (if failed)
        """
        if not self._interaction_recording_active:
            return {
                "success": False,
                "error": "No interaction recording active",
            }

        try:
            session_id = self._interaction_session_id
            start_time = self._interaction_start_time

            # Stop input monitoring
            events_file = None
            events_count = 0
            if self.input_monitor_service:
                events_file = self.input_monitor_service.stop_monitoring()
                events_count = len(self.input_monitor_service.get_events())

            # Calculate duration
            duration = time.time() - start_time if start_time else 0

            # Reset state
            self._interaction_recording_active = False
            self._interaction_session_id = None
            self._interaction_start_time = None

            self.event_manager.emit_log(
                "info",
                f"Interaction recording stopped: {events_count} events captured, "
                f"duration={duration:.1f}s, file={events_file}",
            )

            # Emit recording stopped event (use emit_event_wrapper for string event types)
            self.event_manager.emit_event_wrapper(
                "interaction_recording_stopped",
                {
                    "session_id": session_id,
                    "events_file": events_file,
                    "events_count": events_count,
                    "duration": duration,
                },
            )

            return {
                "success": True,
                "session_id": session_id,
                "events_file": events_file,
                "events_count": events_count,
                "duration": duration,
            }

        except Exception as e:
            self.event_manager.emit_log("error", f"Failed to stop interaction recording: {e}")
            import traceback

            traceback.print_exc(file=sys.stderr)
            # Reset state even on error
            self._interaction_recording_active = False
            return {"success": False, "error": str(e)}

    def _handle_get_interaction_recording_status(self) -> dict[str, Any]:
        """Get current interaction recording status.

        Returns:
            Dictionary with:
                - success: Always True
                - is_recording: Whether recording is active
                - session_id: Current session ID (if recording)
                - duration: Current recording duration in seconds (if recording)
                - events_count: Number of events captured so far (if recording)
        """
        if not self._interaction_recording_active:
            return {
                "success": True,
                "is_recording": False,
            }

        duration = time.time() - self._interaction_start_time if self._interaction_start_time else 0
        events_count = 0
        if self.input_monitor_service:
            events_count = len(self.input_monitor_service.get_events())

        return {
            "success": True,
            "is_recording": True,
            "session_id": self._interaction_session_id,
            "duration": duration,
            "events_count": events_count,
            "fps": self._interaction_fps,
        }
