#!/usr/bin/env python3
"""
Minimal bridge between Tauri and Qontinui library.
All automation logic is delegated to Qontinui's API.
The library handles both real and mock execution modes.
"""

import json
import sys
import tempfile
import threading
import time
import traceback
from enum import Enum
from typing import Any

# Import Qontinui library - REQUIRED (no fallback)
from qontinui.json_executor.json_runner import JSONRunner
from qontinui.mock import MockModeManager


class EventType(Enum):
    """Event types for Tauri communication."""

    READY = "ready"
    CONFIG_LOADED = "config_loaded"
    EXECUTION_STARTED = "execution_started"
    STATE_DETECTED = "state_detected"
    ACTION_STARTED = "action_started"
    ACTION_COMPLETED = "action_completed"
    PROCESS_STARTED = "process_started"
    PROCESS_COMPLETED = "process_completed"
    EXECUTION_COMPLETED = "execution_completed"
    ERROR = "error"
    LOG = "log"


class QontinuiBridge:
    """Minimal bridge that delegates all logic to Qontinui."""

    def __init__(self, mock_mode: bool = False):
        # Configure Qontinui's mock mode based on executor type
        MockModeManager.set_mock_mode(mock_mode)

        self.runner = JSONRunner()
        self._sequence = 0
        self._execution_thread = None
        self._is_running = False
        self._temp_config_file = None
        self._setup_callbacks()

        mode_str = "mock/simulation" if mock_mode else "real"
        self._emit_event(
            EventType.READY, {"message": f"Qontinui bridge initialized in {mode_str} mode"}
        )

    def _setup_callbacks(self):
        """Register Qontinui callbacks to emit Tauri events."""
        # These callbacks will be implemented when Qontinui supports them
        if hasattr(self.runner, "on_state_change"):
            self.runner.on_state_change = lambda data: self._emit_event(
                EventType.STATE_DETECTED, data
            )
        if hasattr(self.runner, "on_action_start"):
            self.runner.on_action_start = lambda data: self._emit_event(
                EventType.ACTION_STARTED, data
            )
        if hasattr(self.runner, "on_action_complete"):
            self.runner.on_action_complete = lambda data: self._emit_event(
                EventType.ACTION_COMPLETED, data
            )
        if hasattr(self.runner, "on_process_start"):
            self.runner.on_process_start = lambda data: self._emit_event(
                EventType.PROCESS_STARTED, data
            )
        if hasattr(self.runner, "on_process_complete"):
            self.runner.on_process_complete = lambda data: self._emit_event(
                EventType.PROCESS_COMPLETED, data
            )

    def _emit_event(self, event_type: EventType, data: dict[str, Any]):
        """Emit event to Tauri through stdout."""
        event = {
            "type": "event",
            "event": event_type.value,
            "timestamp": time.time(),
            "sequence": self._sequence,
            "data": data,
        }
        self._sequence += 1
        sys.stdout.write(json.dumps(event) + "\n")
        sys.stdout.flush()

    def _emit_log(self, level: str, message: str):
        """Emit log message."""
        self._emit_event(EventType.LOG, {"level": level, "message": message})

    def handle_command(self, command: dict[str, Any]) -> dict[str, Any]:
        """Handle command from Tauri - all delegated to Qontinui."""
        cmd_type = command.get("command")
        params = command.get("params", {})

        try:
            if cmd_type == "load":
                return self._handle_load(params)
            elif cmd_type == "start":
                return self._handle_start(params)
            elif cmd_type == "stop":
                return self._handle_stop()
            elif cmd_type == "status":
                return self._handle_status()
            else:
                return {"success": False, "error": f"Unknown command: {cmd_type}"}

        except Exception as e:
            self._emit_event(
                EventType.ERROR,
                {
                    "message": "Command execution failed",
                    "details": str(e),
                    "traceback": traceback.format_exc(),
                },
            )
            return {"success": False, "error": str(e)}

    def _handle_load(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle configuration loading."""
        try:
            # Get configuration data
            config_data = params.get("config_data")
            if not config_data:
                # Try loading from file path (backward compatibility)
                config_path = params.get("config_path")
                if config_path:
                    with open(config_path) as f:
                        config_data = f.read()
                else:
                    return {"success": False, "error": "No configuration provided"}

            # JSONRunner expects a file path, so save config to temp file
            self._emit_log("info", "Loading configuration into Qontinui")

            # Clean up any previous temp file
            if self._temp_config_file:
                try:
                    import os

                    os.unlink(self._temp_config_file.name)
                except Exception:
                    pass

            # Create temp file with config data
            with tempfile.NamedTemporaryFile(mode="w", suffix=".json", delete=False) as f:
                f.write(config_data)
                self._temp_config_file = f

            success = self.runner.load_configuration(self._temp_config_file.name)

            if success:
                # Parse config to get metadata for event
                try:
                    config = json.loads(config_data)
                    config_info = {
                        "version": config.get("version", "unknown"),
                        "name": config.get("metadata", {}).get("name", "Unnamed"),
                        "states": len(config.get("states", [])),
                        "processes": len(config.get("processes", [])),
                        "transitions": len(config.get("transitions", [])),
                        "images": len(config.get("images", [])),
                    }
                    self._emit_event(EventType.CONFIG_LOADED, config_info)
                except Exception:
                    self._emit_event(EventType.CONFIG_LOADED, {"name": "Configuration loaded"})

            return {"success": success}
        except Exception:
            raise

    def _handle_start(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle execution start."""
        try:
            # Get execution mode
            mode = params.get("mode", "state_machine")
            process_id = params.get("process_id")
            monitor_index = params.get("monitor_index", 0)  # Default to primary monitor

            if not self.runner.config:
                self._emit_event(EventType.ERROR, {"message": "No configuration loaded"})
                return {"success": False, "error": "No configuration loaded"}

            if self._is_running:
                self._emit_event(EventType.ERROR, {"message": "Execution already in progress"})
                return {"success": False, "error": "Already running"}

            self._emit_event(
                EventType.EXECUTION_STARTED,
                {"mode": mode, "process_id": process_id, "monitor_index": monitor_index},
            )

            # Run in separate thread to not block
            def run_automation():
                try:
                    self._is_running = True

                    self._emit_log("info", f"Using monitor {monitor_index} for automation")

                    # Pass monitor_index to JSONRunner.run()
                    success = self.runner.run(mode=mode, monitor_index=monitor_index)

                    self._emit_event(
                        EventType.EXECUTION_COMPLETED, {"success": success, "mode": mode}
                    )
                except Exception as e:
                    self._emit_event(
                        EventType.ERROR, {"message": "Execution failed", "details": str(e)}
                    )
                finally:
                    self._is_running = False

            self._execution_thread = threading.Thread(target=run_automation)
            self._execution_thread.daemon = True
            self._execution_thread.start()

            return {"success": True}
        except Exception:
            raise

    def _handle_stop(self) -> dict[str, Any]:
        """Handle execution stop."""
        try:
            # JSONRunner doesn't have stop(), so we'll track it ourselves
            self._emit_log("info", "Stopping execution...")
            self._is_running = False
            self._emit_event(
                EventType.EXECUTION_COMPLETED, {"success": False, "reason": "User stopped"}
            )
            return {"success": True}
        except Exception:
            raise

    def _handle_status(self) -> dict[str, Any]:
        """Handle status request."""
        try:
            # Get status from our tracking and JSONRunner
            current_state = None
            if self.runner.state_executor and hasattr(self.runner.state_executor, "current_state"):
                current_state = self.runner.state_executor.current_state

            return {
                "is_running": self._is_running,
                "current_state": current_state,
                "config_loaded": self.runner.config is not None,
            }
        except Exception:
            raise


def main():
    """Main entry point - minimal event loop."""
    # Check if mock mode is requested via command line argument
    mock_mode = "--mock" in sys.argv or "--simulation" in sys.argv

    # Initialize bridge with appropriate mode
    bridge = QontinuiBridge(mock_mode=mock_mode)

    # Simple stdin reader - all logic in Qontinui
    for line in sys.stdin:
        try:
            command = json.loads(line.strip())

            if command.get("type") == "command":
                response = bridge.handle_command(command)
                response["id"] = command.get("id")
                response["type"] = "response"
                sys.stdout.write(json.dumps(response) + "\n")
                sys.stdout.flush()

        except json.JSONDecodeError as e:
            bridge._emit_event(
                EventType.ERROR, {"message": "Invalid JSON command", "details": str(e)}
            )
        except Exception as e:
            bridge._emit_event(
                EventType.ERROR,
                {
                    "message": "Command execution failed",
                    "details": str(e),
                    "traceback": traceback.format_exc(),
                },
            )


if __name__ == "__main__":
    main()
