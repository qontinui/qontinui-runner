#!/usr/bin/env python3
"""
Minimal bridge for testing Rust-Python communication.
This version doesn't import qontinui to isolate communication issues.
"""

import json
import logging
import sys
import threading
import time
from enum import Enum
from typing import Any

# Configure logging to stderr to avoid print statements
logging.basicConfig(level=logging.INFO, stream=sys.stderr)
logger = logging.getLogger(__name__)


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


class MinimalBridge:
    """Minimal bridge for testing without qontinui dependency."""

    def __init__(self, mock_mode: bool = False):
        self.mock_mode = mock_mode
        self.config = None
        self._sequence = 0
        self._execution_thread = None
        self._is_running = False

        mode_str = "mock/simulation" if mock_mode else "real"
        self._emit_event(
            EventType.READY,
            {"message": f"Minimal bridge initialized in {mode_str} mode (no qontinui)"},
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

    def handle_command(self, command: dict[str, Any]) -> dict[str, Any]:  # noqa: C901
        """Handle command from Tauri."""
        cmd_type = command.get("command")
        params = command.get("params", {})

        try:
            if cmd_type == "load":
                # Simulate loading configuration
                config_data = params.get("config_data")
                config_path = params.get("config_path")

                if config_data:
                    self.config = (
                        json.loads(config_data) if isinstance(config_data, str) else config_data
                    )
                elif config_path:
                    with open(config_path) as f:
                        self.config = json.load(f)
                else:
                    return {"success": False, "error": "No configuration provided"}

                self._emit_log("info", "Configuration loaded (minimal bridge)")

                config_info = {
                    "version": self.config.get("version", "unknown"),
                    "name": self.config.get("metadata", {}).get("name", "Unnamed"),
                    "states": len(self.config.get("states", [])),
                    "processes": len(self.config.get("processes", [])),
                    "transitions": len(self.config.get("transitions", [])),
                    "images": len(self.config.get("images", [])),
                }
                self._emit_event(EventType.CONFIG_LOADED, config_info)

                return {"success": True}

            elif cmd_type == "start":
                mode = params.get("mode", "state_machine")
                process_id = params.get("process_id")
                monitor_index = params.get("monitor_index", 0)  # Default to primary monitor

                if not self.config:
                    self._emit_event(EventType.ERROR, {"message": "No configuration loaded"})
                    return {"success": False, "error": "No configuration loaded"}

                if self._is_running:
                    self._emit_event(EventType.ERROR, {"message": "Execution already in progress"})
                    return {"success": False, "error": "Already running"}

                self._emit_event(
                    EventType.EXECUTION_STARTED,
                    {"mode": mode, "process_id": process_id, "monitor_index": monitor_index},
                )

                # Simulate execution in separate thread
                def simulate_execution():
                    try:
                        self._is_running = True
                        self._emit_log(
                            "info",
                            f"Starting simulated execution in {mode} mode on monitor {monitor_index}",
                        )

                        # Simulate some work
                        for i in range(3):
                            if not self._is_running:
                                break

                            self._emit_event(
                                EventType.STATE_DETECTED,
                                {"state": f"state_{i}", "confidence": 0.95},
                            )
                            time.sleep(1)

                            self._emit_event(
                                EventType.ACTION_STARTED, {"action": f"action_{i}", "type": "click"}
                            )
                            time.sleep(0.5)

                            self._emit_event(
                                EventType.ACTION_COMPLETED,
                                {"action": f"action_{i}", "success": True},
                            )
                            time.sleep(0.5)

                        self._emit_event(
                            EventType.EXECUTION_COMPLETED, {"success": True, "mode": mode}
                        )
                    except Exception as e:
                        self._emit_event(
                            EventType.ERROR, {"message": "Execution failed", "details": str(e)}
                        )
                    finally:
                        self._is_running = False

                self._execution_thread = threading.Thread(target=simulate_execution)
                self._execution_thread.daemon = True
                self._execution_thread.start()

                return {"success": True}

            elif cmd_type == "stop":
                self._emit_log("info", "Stopping execution...")
                self._is_running = False
                self._emit_event(
                    EventType.EXECUTION_COMPLETED, {"success": False, "reason": "User stopped"}
                )
                return {"success": True}

            elif cmd_type == "status":
                return {
                    "success": True,
                    "is_running": self._is_running,
                    "config_loaded": self.config is not None,
                    "bridge_type": "minimal",
                }

            else:
                return {"success": False, "error": f"Unknown command: {cmd_type}"}

        except Exception as e:
            self._emit_event(
                EventType.ERROR, {"message": "Command execution failed", "details": str(e)}
            )
            return {"success": False, "error": str(e)}


def main():
    """Main entry point."""
    # Check if mock mode is requested
    mock_mode = "--mock" in sys.argv or "--simulation" in sys.argv

    # Initialize bridge
    bridge = MinimalBridge(mock_mode=mock_mode)

    # Read commands from stdin
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
                EventType.ERROR, {"message": "Command execution failed", "details": str(e)}
            )


if __name__ == "__main__":
    main()
