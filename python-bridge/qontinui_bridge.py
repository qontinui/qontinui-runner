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
from qontinui.runner import DSLParser, ExecutionError, StatementExecutor


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
    IMAGE_RECOGNITION = "image_recognition"
    ACTION_EXECUTION = "action_execution"
    # Scheduler events
    SCHEDULER_STARTED = "scheduler_started"
    SCHEDULER_STOPPED = "scheduler_stopped"
    SCHEDULE_TRIGGERED = "schedule_triggered"
    SCHEDULE_EXECUTION_STARTED = "schedule_execution_started"
    SCHEDULE_EXECUTION_COMPLETED = "schedule_execution_completed"
    STATE_CHECK_PERFORMED = "state_check_performed"
    # DSL events
    DSL_IF_BRANCH = "dsl_if_branch"
    DSL_LOOP_ITERATION = "dsl_loop_iteration"
    DSL_EXECUTION_ERROR = "dsl_execution_error"


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
        self._scheduler_running = False
        self._dsl_parser = DSLParser()
        self._dsl_executor = None
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
            elif cmd_type == "get_monitors":
                return self._handle_get_monitors()
            elif cmd_type == "scheduler_start":
                return self._handle_scheduler_start(params)
            elif cmd_type == "scheduler_stop":
                return self._handle_scheduler_stop()
            elif cmd_type == "scheduler_status":
                return self._handle_scheduler_status()
            elif cmd_type == "scheduler_get_statistics":
                return self._handle_scheduler_get_statistics()
            elif cmd_type == "execute_dsl":
                return self._handle_execute_dsl(params)
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

    def _handle_get_monitors(self) -> dict[str, Any]:
        """Handle monitor detection request."""
        try:
            # Get monitor count from JSONRunner's monitor manager
            monitor_count = 1  # Default
            monitor_indices = [0]  # Default to single monitor

            if hasattr(self.runner, "monitor_manager") and self.runner.monitor_manager:
                monitor_count = self.runner.monitor_manager.get_monitor_count()
                monitor_indices = list(range(monitor_count))
                self._emit_log("info", f"Detected {monitor_count} monitor(s) from qontinui")

            return {
                "success": True,
                "count": monitor_count,
                "indices": monitor_indices,
            }
        except Exception:
            raise

    def _handle_start(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle execution start."""
        try:
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
                {"process_id": process_id, "monitor_index": monitor_index},
            )

            # Run in separate thread to not block
            def run_automation():
                try:
                    self._is_running = True

                    self._emit_log("info", f"Using monitor {monitor_index} for automation")
                    self._emit_log("info", f"Starting process: {process_id}")

                    # Pass process_id and monitor_index to JSONRunner.run()
                    success = self.runner.run(process_id=process_id, monitor_index=monitor_index)

                    self._emit_log("info", f"Execution completed with success={success}")
                    self._emit_event(EventType.EXECUTION_COMPLETED, {"success": success})
                except Exception as e:
                    error_details = f"{str(e)}\n{traceback.format_exc()}"
                    self._emit_log("error", f"Execution failed: {error_details}")
                    self._emit_event(
                        EventType.ERROR, {"message": "Execution failed", "details": error_details}
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
            self._emit_log("info", "Stopping execution...")
            self._is_running = False

            # Request the runner to stop gracefully
            if self.runner:
                self.runner.request_stop()

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

    def _handle_scheduler_start(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle scheduler start command."""
        try:
            if not self.runner.scheduler_executor:
                return {"success": False, "error": "No schedules configured"}

            if self._scheduler_running:
                return {"success": False, "error": "Scheduler already running"}

            self._emit_log("info", "Starting scheduler...")
            self.runner.start_scheduler()
            self._scheduler_running = True

            # Get schedule information
            schedule_count = len(self.runner.config.schedules) if self.runner.config else 0
            active_count = sum(
                1 for s in (self.runner.config.schedules if self.runner.config else []) if s.enabled
            )

            self._emit_event(
                EventType.SCHEDULER_STARTED,
                {
                    "total_schedules": schedule_count,
                    "active_schedules": active_count,
                },
            )

            return {
                "success": True,
                "total_schedules": schedule_count,
                "active_schedules": active_count,
            }
        except Exception:
            raise

    def _handle_scheduler_stop(self) -> dict[str, Any]:
        """Handle scheduler stop command."""
        try:
            if not self._scheduler_running:
                return {"success": False, "error": "Scheduler not running"}

            self._emit_log("info", "Stopping scheduler...")
            self.runner.stop_scheduler()
            self._scheduler_running = False

            self._emit_event(EventType.SCHEDULER_STOPPED, {})

            return {"success": True}
        except Exception:
            raise

    def _handle_scheduler_status(self) -> dict[str, Any]:
        """Handle scheduler status request."""
        try:
            has_scheduler = self.runner.scheduler_executor is not None
            schedule_count = (
                len(self.runner.config.schedules) if self.runner.config and has_scheduler else 0
            )
            active_count = 0

            if has_scheduler and self.runner.config:
                active_count = sum(1 for s in self.runner.config.schedules if s.enabled)

            return {
                "success": True,
                "scheduler_running": self._scheduler_running,
                "has_schedules": schedule_count > 0,
                "total_schedules": schedule_count,
                "active_schedules": active_count,
            }
        except Exception:
            raise

    def _handle_scheduler_get_statistics(self) -> dict[str, Any]:
        """Handle scheduler statistics request."""
        try:
            if not self.runner.scheduler_executor:
                return {
                    "success": True,
                    "statistics": {
                        "total_schedules": 0,
                        "active_schedules": 0,
                        "total_executions": 0,
                        "successful_executions": 0,
                        "failed_executions": 0,
                        "average_iteration_count": 0.0,
                    },
                }

            stats = self.runner.get_scheduler_statistics()

            return {
                "success": True,
                "statistics": stats,
            }
        except Exception:
            raise

    def _handle_execute_dsl(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle DSL execution request.

        Executes DSL instruction sets with control flow support.

        Args:
            params: Dictionary with:
                - dsl_json: JSON string with DSL instruction set

        Returns:
            Execution result with success status and return value
        """
        try:
            dsl_json = params.get("dsl_json")
            if not dsl_json:
                return {"success": False, "error": "No DSL JSON provided"}

            self._emit_log("info", "Parsing DSL instruction set")

            # Parse DSL JSON
            instruction_set = self._dsl_parser.parse_json(dsl_json)

            if not instruction_set.automation_functions:
                return {"success": False, "error": "No automation functions in DSL"}

            # Initialize DSL executor with action executor bridge
            # For now, we'll create a simple action executor that can call Qontinui actions
            from .dsl_action_executor import DSLActionExecutor

            action_executor = DSLActionExecutor(self.runner)
            self._dsl_executor = StatementExecutor(action_executor=action_executor)

            # Execute each automation function
            results = []
            for func in instruction_set.automation_functions:
                try:
                    self._emit_log("info", f"Executing automation function: {func.name}")

                    # Execute function statements
                    return_value = None
                    try:
                        for statement in func.statements:
                            # Execute statement using the executor's execute method
                            self._dsl_executor.execute(statement)

                    except Exception as e:
                        from qontinui.runner.dsl.executor.flow_control import ReturnException

                        if isinstance(e, ReturnException):
                            return_value = e.value
                        else:
                            raise

                    results.append(
                        {
                            "function": func.name,
                            "success": True,
                            "return_value": return_value,
                        }
                    )

                except ExecutionError as e:
                    self._emit_event(
                        EventType.DSL_EXECUTION_ERROR,
                        {
                            "function": func.name,
                            "error": str(e),
                            "statement_type": e.statement_type,
                            "context": e.context,
                        },
                    )
                    results.append(
                        {
                            "function": func.name,
                            "success": False,
                            "error": str(e),
                        }
                    )
                except Exception as e:
                    self._emit_event(
                        EventType.DSL_EXECUTION_ERROR,
                        {
                            "function": func.name,
                            "error": str(e),
                        },
                    )
                    results.append(
                        {
                            "function": func.name,
                            "success": False,
                            "error": str(e),
                        }
                    )

            overall_success = all(r["success"] for r in results)
            return {
                "success": overall_success,
                "results": results,
            }

        except Exception as e:
            error_msg = f"DSL execution failed: {str(e)}"
            self._emit_log("error", error_msg)
            self._emit_event(
                EventType.DSL_EXECUTION_ERROR,
                {
                    "error": str(e),
                    "traceback": traceback.format_exc(),
                },
            )
            return {
                "success": False,
                "error": str(e),
                "traceback": traceback.format_exc(),
            }


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
