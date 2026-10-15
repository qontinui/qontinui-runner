"""Integration-testing commands (``testing_*``).

A mixin of ``QontinuiExecutor``. Its methods were moved verbatim from
``qontinui_executor.py`` by plan
2026-10-04-runner-python-executor-routes-118-commands-through-one-if-chain
and still read executor state through ``self``. ``COMMANDS`` maps each command
name to its handler method.
"""

from typing import Any, ClassVar

from ._host import ExecutorHost
from ._shared import IntegrationTestingService


class TestingCommands(ExecutorHost):
    """Integration-testing commands (``testing_*``)."""

    COMMANDS: ClassVar[dict[str, str]] = {
        # Integration testing commands
        "testing_get_states": "_handle_testing_get_states",
        "testing_get_transitions": "_handle_testing_get_transitions",
        "testing_find_path": "_handle_testing_find_path",
        "testing_traverse_to_state": "_handle_testing_traverse_to_state",
        "testing_get_active_states": "_handle_testing_get_active_states",
        "testing_set_mock_mode": "_handle_testing_set_mock_mode",
        "testing_mock_click": "_handle_testing_mock_click",
        "testing_mock_type": "_handle_testing_mock_type",
        "testing_mock_screenshot": "_handle_testing_mock_screenshot",
        "testing_get_mocked_actions": "_handle_testing_get_mocked_actions",
        "testing_clear_mocked_actions": "_handle_testing_clear_mocked_actions",
        "testing_start_run": "_handle_testing_start_run",
        "testing_run_assertion": "_handle_testing_run_assertion",
        "testing_run_test_case": "_handle_testing_run_test_case",
        "testing_end_run": "_handle_testing_end_run",
        "testing_get_run": "_handle_testing_get_run",
        "testing_get_status": "_handle_testing_get_status",
        "testing_get_results": "_handle_testing_get_results",
        "testing_list_runs": "_handle_testing_list_runs",
    }

    # =========================================================================
    # Integration Testing Handlers
    # =========================================================================

    def _get_integration_testing_service(self) -> IntegrationTestingService:
        """Get or create the integration testing service."""
        if self._integration_testing_service is None:
            self._integration_testing_service = IntegrationTestingService(
                emit_log_fn=self.event_manager.emit_log,
                emit_event_fn=self.event_manager.emit_event,
            )
        return self._integration_testing_service  # type: ignore[no-any-return]

    def _handle_testing_get_states(self) -> dict[str, Any]:
        """Handle get states command for integration testing."""
        service = self._get_integration_testing_service()
        states = service.get_states()
        return {"success": True, "states": states}

    def _handle_testing_get_transitions(self) -> dict[str, Any]:
        """Handle get transitions command for integration testing."""
        service = self._get_integration_testing_service()
        transitions = service.get_transitions()
        return {"success": True, "transitions": transitions}

    def _handle_testing_find_path(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle find path command for integration testing."""
        service = self._get_integration_testing_service()
        from_state = params.get("from_state", "")
        to_state = params.get("to_state", "")

        if not from_state or not to_state:
            return {"success": False, "error": "from_state and to_state are required"}

        result = service.find_path(from_state, to_state)
        return result

    def _handle_testing_traverse_to_state(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle traverse to state command for integration testing."""
        service = self._get_integration_testing_service()
        target_state = params.get("target_state", "")
        execute = params.get("execute", True)

        if not target_state:
            return {"success": False, "error": "target_state is required"}

        result = service.traverse_to_state(target_state, execute=execute)
        return result

    def _handle_testing_get_active_states(self) -> dict[str, Any]:
        """Handle get active states command for integration testing."""
        service = self._get_integration_testing_service()
        active_states = service.get_active_states()
        return {"success": True, "active_states": active_states}

    def _handle_testing_set_mock_mode(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle set mock mode command for integration testing."""
        service = self._get_integration_testing_service()
        mode = params.get("mode", "disabled")
        return service.set_mock_mode(mode)

    def _handle_testing_mock_click(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle mock click command for integration testing."""
        service = self._get_integration_testing_service()
        x = params.get("x", 0)
        y = params.get("y", 0)
        button = params.get("button", "left")
        clicks = params.get("clicks", 1)
        return service.mock_click(x, y, button, clicks)

    def _handle_testing_mock_type(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle mock type command for integration testing."""
        service = self._get_integration_testing_service()
        text = params.get("text", "")
        delay_ms = params.get("delay_ms", 50)
        return service.mock_type(text, delay_ms)

    def _handle_testing_mock_screenshot(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle mock screenshot command for integration testing."""
        service = self._get_integration_testing_service()
        monitor_index = params.get("monitor_index")
        return service.mock_screenshot(monitor_index)

    def _handle_testing_get_mocked_actions(self) -> dict[str, Any]:
        """Handle get mocked actions command for integration testing."""
        service = self._get_integration_testing_service()
        actions = service.get_mocked_actions()
        return {"success": True, "actions": actions}

    def _handle_testing_clear_mocked_actions(self) -> dict[str, Any]:
        """Handle clear mocked actions command for integration testing."""
        service = self._get_integration_testing_service()
        return service.clear_mocked_actions()

    def _handle_testing_start_run(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle start test run command for integration testing."""
        service = self._get_integration_testing_service()
        name = params.get("name", "Test Run")
        config_path = params.get("config_path")
        metadata = params.get("metadata")
        return service.start_test_run(name, config_path, metadata)

    def _handle_testing_run_assertion(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle run assertion command for integration testing."""
        service = self._get_integration_testing_service()
        assertion_type = params.get("assertion_type", "")
        target = params.get("target", "")
        expected = params.get("expected")
        timeout_seconds = params.get("timeout_seconds", 30.0)

        if not assertion_type or not target:
            return {"success": False, "error": "assertion_type and target are required"}

        return service.run_assertion(assertion_type, target, expected, timeout_seconds)

    def _handle_testing_run_test_case(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle run test case command for integration testing."""
        service = self._get_integration_testing_service()
        test_case = params.get("test_case", {})
        return service.run_test_case(test_case)

    def _handle_testing_end_run(self) -> dict[str, Any]:
        """Handle end test run command for integration testing."""
        service = self._get_integration_testing_service()
        return service.end_test_run()

    def _handle_testing_get_run(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle get test run command for integration testing."""
        service = self._get_integration_testing_service()
        run_id = params.get("run_id", "")
        if not run_id:
            return {"success": False, "error": "run_id is required"}
        return service.get_test_run(run_id)

    def _handle_testing_get_status(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle get test status command for integration testing."""
        service = self._get_integration_testing_service()
        run_id = params.get("run_id", "")
        if not run_id:
            return {"success": False, "error": "run_id is required"}
        return service.get_test_status(run_id)

    def _handle_testing_get_results(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle get test results command for integration testing."""
        service = self._get_integration_testing_service()
        run_id = params.get("run_id", "")
        if not run_id:
            return {"success": False, "error": "run_id is required"}
        return service.get_test_results(run_id)

    def _handle_testing_list_runs(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle list test runs command for integration testing."""
        service = self._get_integration_testing_service()
        limit = params.get("limit", 50)
        runs = service.list_test_runs(limit)
        return {"success": True, "runs": runs}
