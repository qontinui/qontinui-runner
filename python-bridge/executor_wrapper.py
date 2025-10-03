#!/usr/bin/env python3
"""
Wrapper for Qontinui JSONRunner that adds event emission for Tauri communication.
"""

import json
import sys
import traceback
from enum import Enum
from pathlib import Path
from typing import Any

# Add parent directory to path to import qontinui
sys.path.insert(0, str(Path(__file__).parent.parent.parent / "qontinui" / "src"))

from qontinui.json_executor import JSONRunner
from qontinui.json_executor.config_parser import QontinuiConfig


class EventType(Enum):
    """Event types for communication with Tauri."""

    READY = "ready"
    CONFIG_LOADED = "config_loaded"
    EXECUTION_STARTED = "execution_started"
    STATE_CHANGED = "state_changed"
    ACTION_STARTED = "action_started"
    ACTION_COMPLETED = "action_completed"
    PROCESS_STARTED = "process_started"
    PROCESS_COMPLETED = "process_completed"
    EXECUTION_COMPLETED = "execution_completed"
    ERROR = "error"
    LOG = "log"
    SCREENSHOT = "screenshot"


class ExecutorWrapper:
    """Wraps JSONRunner with event emission capabilities."""

    def __init__(self):
        self.runner: JSONRunner | None = None
        self.config: QontinuiConfig | None = None
        self.current_state = None
        self.is_running = False
        self._emit_event(EventType.READY, {"message": "Executor initialized"})

    def _emit_event(self, event_type: EventType, data: dict[str, Any]):
        """Emit event to Tauri through stdout."""
        event = {"type": "event", "event": event_type.value, "data": data}
        sys.stdout.write(json.dumps(event) + "\n")
        sys.stdout.flush()

    def _emit_log(self, level: str, message: str):
        """Emit log message."""
        self._emit_event(EventType.LOG, {"level": level, "message": message})

    def load_configuration(self, config_path: str) -> bool:
        """Load configuration from file."""
        try:
            self._emit_log("info", f"Loading configuration from: {config_path}")

            self.runner = JSONRunner(config_path)
            success = self.runner.load_configuration()

            if success:
                self.config = self.runner.config
                config_info = {
                    "path": config_path,
                    "version": self.config.version,
                    "name": self.config.metadata.get("name", "Unnamed"),
                    "states": len(self.config.states),
                    "processes": len(self.config.processes),
                    "transitions": len(self.config.transitions),
                    "images": len(self.config.images),
                }
                self._emit_event(EventType.CONFIG_LOADED, config_info)
                return True
            else:
                self._emit_event(
                    EventType.ERROR,
                    {
                        "message": "Failed to load configuration",
                        "details": "Invalid configuration format",
                    },
                )
                return False

        except Exception as e:
            self._emit_event(
                EventType.ERROR,
                {
                    "message": "Exception loading configuration",
                    "details": str(e),
                    "traceback": traceback.format_exc(),
                },
            )
            return False

    def start_execution(self, mode: str = "state_machine") -> bool:
        """Start automation execution."""
        if not self.runner or not self.config:
            self._emit_event(EventType.ERROR, {"message": "No configuration loaded"})
            return False

        if self.is_running:
            self._emit_event(EventType.ERROR, {"message": "Execution already in progress"})
            return False

        try:
            self.is_running = True
            self._emit_event(
                EventType.EXECUTION_STARTED,
                {"mode": mode, "initial_state": self._get_initial_state_id()},
            )

            # Hook into state executor to emit events
            self._setup_execution_hooks()

            # Run the automation
            success = self.runner.run(mode)

            self._emit_event(
                EventType.EXECUTION_COMPLETED,
                {"success": success, "final_state": self.current_state},
            )

            return success

        except Exception as e:
            self._emit_event(
                EventType.ERROR,
                {
                    "message": "Execution failed",
                    "details": str(e),
                    "traceback": traceback.format_exc(),
                },
            )
            return False
        finally:
            self.is_running = False

    def stop_execution(self):
        """Stop the current execution."""
        if self.is_running and self.runner:
            self._emit_log("info", "Stopping execution...")
            # TODO: Implement graceful shutdown
            self.is_running = False
            self._emit_event(
                EventType.EXECUTION_COMPLETED, {"success": False, "reason": "User stopped"}
            )

    def _get_initial_state_id(self) -> str | None:
        """Get the initial state ID."""
        if not self.config:
            return None

        for state in self.config.states:
            if state.is_initial:
                return state.id

        # If no initial state marked, use first
        if self.config.states:
            return self.config.states[0].id

        return None

    def _setup_execution_hooks(self):
        """Set up hooks to emit events during execution."""
        if not self.runner or not self.runner.state_executor:
            return

        # Monkey-patch the state executor to emit events
        original_execute_state = self.runner.state_executor.execute_state

        def hooked_execute_state(state):
            self.current_state = state.id
            self._emit_event(
                EventType.STATE_CHANGED, {"state_id": state.id, "state_name": state.name}
            )
            return original_execute_state(state)

        self.runner.state_executor.execute_state = hooked_execute_state

        # Hook action executor if available
        if hasattr(self.runner, "action_executor"):
            self._hook_action_executor()

    def _hook_action_executor(self):
        """Hook into action executor for action events."""
        # This would hook into the actual action execution
        # For now, we'll just log the intent
        self._emit_log("debug", "Action executor hooks would be set up here")

    def handle_command(self, command: dict[str, Any]) -> dict[str, Any]:
        """Handle command from Tauri."""
        cmd_type = command.get("command")
        params = command.get("params", {})

        if cmd_type == "load":
            config_path = params.get("config_path")
            success = self.load_configuration(config_path)
            return {"success": success}

        elif cmd_type == "start":
            mode = params.get("mode", "state_machine")
            success = self.start_execution(mode)
            return {"success": success}

        elif cmd_type == "stop":
            self.stop_execution()
            return {"success": True}

        elif cmd_type == "status":
            return {
                "is_running": self.is_running,
                "current_state": self.current_state,
                "config_loaded": self.config is not None,
            }

        else:
            return {"success": False, "error": f"Unknown command: {cmd_type}"}


def main():
    """Main entry point for the executor wrapper."""
    wrapper = ExecutorWrapper()

    # Read commands from stdin
    for line in sys.stdin:
        try:
            command = json.loads(line.strip())

            if command.get("type") == "command":
                response = wrapper.handle_command(command)
                response["id"] = command.get("id")
                response["type"] = "response"
                sys.stdout.write(json.dumps(response) + "\n")
                sys.stdout.flush()

        except json.JSONDecodeError as e:
            wrapper._emit_event(
                EventType.ERROR, {"message": "Invalid JSON command", "details": str(e)}
            )
        except Exception as e:
            wrapper._emit_event(
                EventType.ERROR,
                {
                    "message": "Command execution failed",
                    "details": str(e),
                    "traceback": traceback.format_exc(),
                },
            )


if __name__ == "__main__":
    main()
