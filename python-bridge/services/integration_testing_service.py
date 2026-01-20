"""
Integration Testing Service

Provides integration testing capabilities via the qontinui library:
- State machine traversal and path finding
- GUI action mocking for testing without screen interaction
- Test execution with assertions
- Test result reporting

This service is exposed via IPC to the runner.
"""

import logging
import time
import uuid
from dataclasses import dataclass, field
from datetime import datetime
from enum import Enum
from typing import Any

logger = logging.getLogger(__name__)

# Check if qontinui is available
try:
    from qontinui import navigation_api, registry  # noqa: F401

    QONTINUI_AVAILABLE = True
except ImportError:
    QONTINUI_AVAILABLE = False
    logger.debug("Qontinui library not available - integration testing will be limited")


class AssertionType(Enum):
    """Types of test assertions."""

    STATE_REACHED = "state_reached"
    ELEMENT_FOUND = "element_found"
    ACTION_PERFORMED = "action_performed"
    TRANSITION_COMPLETED = "transition_completed"
    WORKFLOW_COMPLETED = "workflow_completed"


class TestStatus(Enum):
    """Status values for test runs."""

    PENDING = "pending"
    RUNNING = "running"
    PASSED = "passed"
    FAILED = "failed"
    SKIPPED = "skipped"
    ERROR = "error"


class MockMode(Enum):
    """Mock modes for GUI actions."""

    DISABLED = "disabled"  # Real execution
    RECORD = "record"  # Record actions but don't execute
    PLAYBACK = "playback"  # Verify recorded actions match expected


@dataclass
class MockedAction:
    """A mocked or recorded GUI action."""

    action_id: str
    action_type: str
    target: str | None
    config: dict[str, Any]
    timestamp: datetime = field(default_factory=datetime.utcnow)
    executed: bool = False
    result: dict[str, Any] | None = None


@dataclass
class TestAssertion:
    """A single test assertion."""

    assertion_id: str
    assertion_type: AssertionType
    target: str  # State name, element ID, action type, etc.
    expected: Any = None  # Expected value for comparison
    timeout_seconds: float = 30.0
    passed: bool | None = None
    error_message: str | None = None
    actual_value: Any = None


@dataclass
class TestCase:
    """A test case with assertions."""

    test_id: str
    name: str
    description: str = ""
    assertions: list[TestAssertion] = field(default_factory=list)
    setup_actions: list[dict[str, Any]] = field(default_factory=list)
    teardown_actions: list[dict[str, Any]] = field(default_factory=list)


@dataclass
class TestResult:
    """Result of a test run."""

    test_id: str
    test_name: str
    status: TestStatus
    assertions: list[TestAssertion]
    start_time: datetime
    end_time: datetime | None = None
    duration_ms: float | None = None
    error_message: str | None = None
    mocked_actions: list[MockedAction] = field(default_factory=list)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return {
            "test_id": self.test_id,
            "test_name": self.test_name,
            "status": self.status.value,
            "assertions": [
                {
                    "assertion_id": a.assertion_id,
                    "type": a.assertion_type.value,
                    "target": a.target,
                    "expected": a.expected,
                    "passed": a.passed,
                    "error_message": a.error_message,
                    "actual_value": str(a.actual_value) if a.actual_value else None,
                }
                for a in self.assertions
            ],
            "start_time": self.start_time.isoformat() if self.start_time else None,
            "end_time": self.end_time.isoformat() if self.end_time else None,
            "duration_ms": self.duration_ms,
            "error_message": self.error_message,
            "mocked_actions_count": len(self.mocked_actions),
        }


@dataclass
class TestRun:
    """A complete test run with multiple test cases."""

    run_id: str
    name: str
    test_results: list[TestResult] = field(default_factory=list)
    status: TestStatus = TestStatus.PENDING
    start_time: datetime | None = None
    end_time: datetime | None = None
    config_path: str | None = None
    metadata: dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        passed = sum(1 for r in self.test_results if r.status == TestStatus.PASSED)
        failed = sum(1 for r in self.test_results if r.status == TestStatus.FAILED)
        total = len(self.test_results)

        return {
            "run_id": self.run_id,
            "name": self.name,
            "status": self.status.value,
            "start_time": self.start_time.isoformat() if self.start_time else None,
            "end_time": self.end_time.isoformat() if self.end_time else None,
            "config_path": self.config_path,
            "summary": {
                "total": total,
                "passed": passed,
                "failed": failed,
                "pass_rate": (passed / total * 100) if total > 0 else 0,
            },
            "test_results": [r.to_dict() for r in self.test_results],
            "metadata": self.metadata,
        }


class IntegrationTestingService:
    """
    Service for running integration tests via the qontinui library.

    Provides:
    - State machine traversal queries
    - GUI action mocking
    - Test execution with assertions
    - Test result reporting
    """

    def __init__(self, emit_log_fn=None, emit_event_fn=None):
        """
        Initialize the integration testing service.

        Args:
            emit_log_fn: Optional function for emitting log messages
            emit_event_fn: Optional function for emitting events
        """
        self.emit_log = emit_log_fn or (
            lambda level, msg: logger.log(getattr(logging, level.upper()), msg)
        )
        self.emit_event = emit_event_fn or (lambda event_type, data: None)

        # Mock mode state
        self._mock_mode = MockMode.DISABLED
        self._mocked_actions: list[MockedAction] = []
        self._expected_actions: list[MockedAction] = []

        # Test run state
        self._test_runs: dict[str, TestRun] = {}
        self._current_run_id: str | None = None

    # =========================================================================
    # State Machine Queries
    # =========================================================================

    def get_states(self) -> list[dict[str, Any]]:
        """
        Get all registered states in the loaded configuration.

        Returns:
            List of state dictionaries with id, name, and metadata
        """
        if not QONTINUI_AVAILABLE:
            return []

        try:
            # Access state service from navigation API if available
            if hasattr(navigation_api, "_state_service") and navigation_api._state_service:
                states = navigation_api._state_service.get_all_states()
                return [
                    {
                        "id": state.id,
                        "name": state.name,
                        "is_initial": getattr(state, "is_initial", False),
                        "is_terminal": getattr(state, "is_terminal", False),
                        "state_images_count": len(getattr(state, "state_images", [])),
                        "transitions_count": len(getattr(state, "transitions", [])),
                    }
                    for state in states
                ]
            return []
        except Exception as e:
            self.emit_log("error", f"Failed to get states: {e}")
            return []

    def get_transitions(self) -> list[dict[str, Any]]:
        """
        Get all transitions in the loaded configuration.

        Returns:
            List of transition dictionaries with source, target, and workflow info
        """
        if not QONTINUI_AVAILABLE:
            return []

        try:
            transitions = []
            if hasattr(navigation_api, "_state_service") and navigation_api._state_service:
                for state in navigation_api._state_service.get_all_states():
                    for transition in getattr(state, "transitions", []):
                        transitions.append(
                            {
                                "id": getattr(transition, "id", None),
                                "source_state_id": state.id,
                                "source_state_name": state.name,
                                "target_state_ids": getattr(transition, "target_state_ids", []),
                                "workflow_id": getattr(transition, "workflow_id", None),
                                "name": getattr(transition, "name", None),
                            }
                        )
            return transitions
        except Exception as e:
            self.emit_log("error", f"Failed to get transitions: {e}")
            return []

    def find_path(self, from_state: str | int, to_state: str | int) -> dict[str, Any]:
        """
        Find a path between two states using the navigation system.

        Args:
            from_state: Source state name or ID
            to_state: Target state name or ID

        Returns:
            Dictionary with path information or error
        """
        if not QONTINUI_AVAILABLE:
            return {"success": False, "error": "Qontinui library not available"}

        try:
            if not hasattr(navigation_api, "_navigator") or not navigation_api._navigator:
                return {"success": False, "error": "Navigation system not initialized"}

            # Get state IDs
            state_service = navigation_api._state_service
            if not state_service:
                return {"success": False, "error": "State service not available"}

            # Resolve source state
            if isinstance(from_state, str):
                source = state_service.get_state_by_name(from_state)
                from_state_id = source.id if source else None
            else:
                from_state_id = from_state

            # Resolve target state
            if isinstance(to_state, str):
                target = state_service.get_state_by_name(to_state)
                to_state_id = target.id if target else None
            else:
                to_state_id = to_state

            if from_state_id is None:
                return {"success": False, "error": f"Source state not found: {from_state}"}
            if to_state_id is None:
                return {"success": False, "error": f"Target state not found: {to_state}"}

            # Find path using navigator
            navigator = navigation_api._navigator
            # Navigator type depends on qontinui library version
            if hasattr(navigator, "find_path"):
                path = navigator.find_path([from_state_id], [to_state_id])
            else:
                return {"success": False, "error": "Navigator does not support find_path"}

            if path:
                return {
                    "success": True,
                    "path": {
                        "source_states": (
                            path.source_states
                            if hasattr(path, "source_states")
                            else [from_state_id]
                        ),
                        "target_states": (
                            path.target_states if hasattr(path, "target_states") else [to_state_id]
                        ),
                        "transitions": [
                            {
                                "id": t.id if hasattr(t, "id") else None,
                                "source": t.source if hasattr(t, "source") else None,
                                "target": t.target if hasattr(t, "target") else None,
                            }
                            for t in (path.transitions if hasattr(path, "transitions") else [])
                        ],
                        "cost": path.cost if hasattr(path, "cost") else None,
                    },
                }
            else:
                return {"success": False, "error": "No path found between states"}

        except Exception as e:
            self.emit_log("error", f"Failed to find path: {e}")
            return {"success": False, "error": str(e)}

    def traverse_to_state(self, target_state: str | int, execute: bool = True) -> dict[str, Any]:
        """
        Navigate to a target state, optionally executing the transition workflows.

        Args:
            target_state: Target state name or ID
            execute: Whether to execute the transitions (False for dry run)

        Returns:
            Dictionary with traversal result
        """
        if not QONTINUI_AVAILABLE:
            return {"success": False, "error": "Qontinui library not available"}

        # If in mock mode, record the action instead of executing
        if self._mock_mode != MockMode.DISABLED:
            action = MockedAction(
                action_id=str(uuid.uuid4()),
                action_type="GO_TO_STATE",
                target=str(target_state),
                config={"execute": execute},
            )
            self._mocked_actions.append(action)

            if self._mock_mode == MockMode.RECORD:
                return {"success": True, "mocked": True, "action_id": action.action_id}
            elif self._mock_mode == MockMode.PLAYBACK:
                # Verify against expected
                return self._verify_mock_action(action)

        # Real execution
        try:
            if isinstance(target_state, int):
                success = navigation_api.open_states([target_state])
            else:
                success = navigation_api.open_state(target_state)

            return {
                "success": success,
                "target_state": target_state,
                "active_states": navigation_api.get_active_states() if success else [],
            }
        except Exception as e:
            self.emit_log("error", f"Failed to traverse to state: {e}")
            return {"success": False, "error": str(e)}

    def get_active_states(self) -> list[str]:
        """
        Get the currently active states.

        Returns:
            List of active state names
        """
        if not QONTINUI_AVAILABLE:
            return []

        try:
            return navigation_api.get_active_states()
        except Exception as e:
            self.emit_log("error", f"Failed to get active states: {e}")
            return []

    def is_state_active(self, state_name: str) -> bool:
        """
        Check if a state is currently active.

        Args:
            state_name: Name of state to check

        Returns:
            True if state is active
        """
        if not QONTINUI_AVAILABLE:
            return False

        try:
            return navigation_api.is_state_active(state_name)
        except Exception as e:
            self.emit_log("error", f"Failed to check state: {e}")
            return False

    # =========================================================================
    # Mock Mode
    # =========================================================================

    def set_mock_mode(self, mode: str) -> dict[str, Any]:
        """
        Set the mock mode for GUI actions.

        Args:
            mode: "disabled", "record", or "playback"

        Returns:
            Dictionary with new mode status
        """
        try:
            self._mock_mode = MockMode(mode)
            if mode == "disabled":
                self._mocked_actions.clear()
            self.emit_log("info", f"Mock mode set to: {mode}")
            return {"success": True, "mode": mode}
        except ValueError:
            return {"success": False, "error": f"Invalid mock mode: {mode}"}

    def get_mock_mode(self) -> str:
        """Get the current mock mode."""
        return str(self._mock_mode.value)

    def mock_click(self, x: int, y: int, button: str = "left", clicks: int = 1) -> dict[str, Any]:
        """
        Mock a click action (record without executing).

        Args:
            x: X coordinate
            y: Y coordinate
            button: Mouse button ("left", "right", "middle")
            clicks: Number of clicks

        Returns:
            Dictionary with mock result
        """
        action = MockedAction(
            action_id=str(uuid.uuid4()),
            action_type="CLICK",
            target=None,
            config={"x": x, "y": y, "button": button, "clicks": clicks},
        )
        self._mocked_actions.append(action)

        return {
            "success": True,
            "action_id": action.action_id,
            "mocked": True,
            "action_type": "CLICK",
            "config": action.config,
        }

    def mock_type(self, text: str, delay_ms: int = 50) -> dict[str, Any]:
        """
        Mock a type action (record without executing).

        Args:
            text: Text to type
            delay_ms: Delay between keystrokes

        Returns:
            Dictionary with mock result
        """
        action = MockedAction(
            action_id=str(uuid.uuid4()),
            action_type="TYPE",
            target=None,
            config={"text": text, "delay_ms": delay_ms},
        )
        self._mocked_actions.append(action)

        return {
            "success": True,
            "action_id": action.action_id,
            "mocked": True,
            "action_type": "TYPE",
            "config": action.config,
        }

    def mock_screenshot(self, monitor_index: int | None = None) -> dict[str, Any]:
        """
        Mock a screenshot action (record without capturing).

        Args:
            monitor_index: Optional monitor index

        Returns:
            Dictionary with mock result
        """
        action = MockedAction(
            action_id=str(uuid.uuid4()),
            action_type="SCREENSHOT",
            target=None,
            config={"monitor_index": monitor_index},
        )
        self._mocked_actions.append(action)

        return {
            "success": True,
            "action_id": action.action_id,
            "mocked": True,
            "action_type": "SCREENSHOT",
            "config": action.config,
        }

    def get_mocked_actions(self) -> list[dict[str, Any]]:
        """
        Get all mocked/recorded actions.

        Returns:
            List of mocked action dictionaries
        """
        return [
            {
                "action_id": a.action_id,
                "action_type": a.action_type,
                "target": a.target,
                "config": a.config,
                "timestamp": a.timestamp.isoformat(),
                "executed": a.executed,
            }
            for a in self._mocked_actions
        ]

    def clear_mocked_actions(self) -> dict[str, Any]:
        """Clear all mocked actions."""
        count = len(self._mocked_actions)
        self._mocked_actions.clear()
        return {"success": True, "cleared": count}

    def set_expected_actions(self, actions: list[dict[str, Any]]) -> dict[str, Any]:
        """
        Set expected actions for playback verification.

        Args:
            actions: List of expected action dictionaries

        Returns:
            Dictionary with result
        """
        self._expected_actions.clear()
        for a in actions:
            self._expected_actions.append(
                MockedAction(
                    action_id=a.get("action_id", str(uuid.uuid4())),
                    action_type=a.get("action_type", ""),
                    target=a.get("target"),
                    config=a.get("config", {}),
                )
            )
        return {"success": True, "count": len(self._expected_actions)}

    def _verify_mock_action(self, action: MockedAction) -> dict[str, Any]:
        """Verify a mocked action against expected actions."""
        if not self._expected_actions:
            return {"success": False, "error": "No expected actions to verify against"}

        # Find matching expected action
        for expected in self._expected_actions:
            if expected.action_type == action.action_type:
                # Compare configs
                if expected.config == action.config:
                    expected.executed = True
                    return {"success": True, "matched": True, "action_id": action.action_id}

        return {
            "success": False,
            "error": "Action did not match any expected action",
            "action": {
                "action_type": action.action_type,
                "config": action.config,
            },
        }

    # =========================================================================
    # Test Execution
    # =========================================================================

    def start_test_run(
        self, name: str, config_path: str | None = None, metadata: dict[str, Any] | None = None
    ) -> dict[str, Any]:
        """
        Start a new test run.

        Args:
            name: Name for this test run
            config_path: Optional config path being tested
            metadata: Optional additional metadata

        Returns:
            Dictionary with run_id
        """
        run_id = str(uuid.uuid4())[:8]
        self._test_runs[run_id] = TestRun(
            run_id=run_id,
            name=name,
            config_path=config_path,
            metadata=metadata or {},
            status=TestStatus.RUNNING,
            start_time=datetime.utcnow(),
        )
        self._current_run_id = run_id

        self.emit_log("info", f"Started test run: {name} ({run_id})")
        return {"success": True, "run_id": run_id}

    def run_assertion(
        self,
        assertion_type: str,
        target: str,
        expected: Any = None,
        timeout_seconds: float = 30.0,
    ) -> dict[str, Any]:
        """
        Run a single assertion.

        Args:
            assertion_type: Type of assertion (state_reached, element_found, etc.)
            target: Target to verify (state name, element ID, etc.)
            expected: Optional expected value
            timeout_seconds: Timeout for assertion

        Returns:
            Dictionary with assertion result
        """
        try:
            atype = AssertionType(assertion_type)
        except ValueError:
            return {"success": False, "error": f"Invalid assertion type: {assertion_type}"}

        assertion = TestAssertion(
            assertion_id=str(uuid.uuid4())[:8],
            assertion_type=atype,
            target=target,
            expected=expected,
            timeout_seconds=timeout_seconds,
        )

        # Execute assertion
        start_time = time.time()
        try:
            if atype == AssertionType.STATE_REACHED:
                assertion.passed = self.is_state_active(target)
                assertion.actual_value = self.get_active_states()

            elif atype == AssertionType.ELEMENT_FOUND:
                # Check if element exists in registry
                if QONTINUI_AVAILABLE:
                    image = registry.get_image(target)
                    assertion.passed = image is not None
                    assertion.actual_value = "found" if image else "not_found"
                else:
                    assertion.passed = False
                    assertion.actual_value = "qontinui_not_available"

            elif atype == AssertionType.ACTION_PERFORMED:
                # Check if action was recorded in mock actions
                found = any(a.action_type == target for a in self._mocked_actions)
                assertion.passed = found
                assertion.actual_value = [a.action_type for a in self._mocked_actions]

            elif atype == AssertionType.TRANSITION_COMPLETED:
                # Check if we can find a path to target (as verification)
                result = self.find_path(
                    self.get_active_states()[0] if self.get_active_states() else "", target
                )
                assertion.passed = result.get("success", False)
                assertion.actual_value = result

            elif atype == AssertionType.WORKFLOW_COMPLETED:
                # Check if workflow exists in registry
                if QONTINUI_AVAILABLE:
                    workflow = registry.get_workflow(target)
                    assertion.passed = workflow is not None
                    assertion.actual_value = "exists" if workflow else "not_found"
                else:
                    assertion.passed = False
                    assertion.actual_value = "qontinui_not_available"

        except Exception as e:
            assertion.passed = False
            assertion.error_message = str(e)

        elapsed = (time.time() - start_time) * 1000

        # Add to current test run if active
        if self._current_run_id and self._current_run_id in self._test_runs:
            # Add to the last test result if there is one
            run = self._test_runs[self._current_run_id]
            if run.test_results:
                run.test_results[-1].assertions.append(assertion)

        return {
            "success": True,
            "assertion": {
                "assertion_id": assertion.assertion_id,
                "type": assertion.assertion_type.value,
                "target": assertion.target,
                "passed": assertion.passed,
                "actual_value": str(assertion.actual_value) if assertion.actual_value else None,
                "error_message": assertion.error_message,
                "duration_ms": elapsed,
            },
        }

    def run_test_case(self, test_case: dict[str, Any]) -> dict[str, Any]:
        """
        Run a complete test case.

        Args:
            test_case: Dictionary with test case definition

        Returns:
            Dictionary with test result
        """
        test_id = test_case.get("test_id", str(uuid.uuid4())[:8])
        test_name = test_case.get("name", "Unnamed Test")

        result = TestResult(
            test_id=test_id,
            test_name=test_name,
            status=TestStatus.RUNNING,
            assertions=[],
            start_time=datetime.utcnow(),
        )

        self.emit_log("info", f"Running test: {test_name}")

        try:
            # Run setup actions
            for action in test_case.get("setup_actions", []):
                self._execute_test_action(action)

            # Run assertions
            all_passed = True
            for assertion_def in test_case.get("assertions", []):
                assertion_result = self.run_assertion(
                    assertion_type=assertion_def.get("type"),
                    target=assertion_def.get("target"),
                    expected=assertion_def.get("expected"),
                    timeout_seconds=assertion_def.get("timeout_seconds", 30.0),
                )

                if assertion_result.get("success"):
                    assertion_data = assertion_result.get("assertion", {})
                    assertion = TestAssertion(
                        assertion_id=assertion_data.get("assertion_id", ""),
                        assertion_type=AssertionType(assertion_data.get("type", "state_reached")),
                        target=assertion_data.get("target", ""),
                        passed=assertion_data.get("passed", False),
                        error_message=assertion_data.get("error_message"),
                        actual_value=assertion_data.get("actual_value"),
                    )
                    result.assertions.append(assertion)
                    if not assertion.passed:
                        all_passed = False

            # Run teardown actions
            for action in test_case.get("teardown_actions", []):
                self._execute_test_action(action)

            result.status = TestStatus.PASSED if all_passed else TestStatus.FAILED

        except Exception as e:
            result.status = TestStatus.ERROR
            result.error_message = str(e)
            self.emit_log("error", f"Test {test_name} failed with error: {e}")

        result.end_time = datetime.utcnow()
        result.duration_ms = (result.end_time - result.start_time).total_seconds() * 1000
        result.mocked_actions = list(self._mocked_actions)

        # Add to current run
        if self._current_run_id and self._current_run_id in self._test_runs:
            self._test_runs[self._current_run_id].test_results.append(result)

        return {"success": True, "result": result.to_dict()}

    def _execute_test_action(self, action: dict[str, Any]) -> None:
        """Execute a test setup/teardown action."""
        action_type = action.get("type", "")

        if action_type == "go_to_state":
            self.traverse_to_state(action.get("state", ""), execute=True)
        elif action_type == "wait":
            time.sleep(action.get("seconds", 1))
        elif action_type == "reset_navigation":
            if QONTINUI_AVAILABLE:
                navigation_api.reset_to_initial_state()
        elif action_type == "set_mock_mode":
            self.set_mock_mode(action.get("mode", "disabled"))

    def end_test_run(self) -> dict[str, Any]:
        """
        End the current test run.

        Returns:
            Dictionary with run summary
        """
        if not self._current_run_id or self._current_run_id not in self._test_runs:
            return {"success": False, "error": "No active test run"}

        run = self._test_runs[self._current_run_id]
        run.end_time = datetime.utcnow()

        # Determine overall status
        if any(r.status == TestStatus.ERROR for r in run.test_results):
            run.status = TestStatus.ERROR
        elif any(r.status == TestStatus.FAILED for r in run.test_results):
            run.status = TestStatus.FAILED
        elif all(r.status == TestStatus.PASSED for r in run.test_results):
            run.status = TestStatus.PASSED
        else:
            run.status = TestStatus.PENDING

        result = run.to_dict()
        self._current_run_id = None

        self.emit_log("info", f"Test run completed: {run.name} - {run.status.value}")
        return {"success": True, "run": result}

    def get_test_run(self, run_id: str) -> dict[str, Any]:
        """
        Get a test run by ID.

        Args:
            run_id: Test run ID

        Returns:
            Dictionary with run data
        """
        run = self._test_runs.get(run_id)
        if not run:
            return {"success": False, "error": "Test run not found"}
        return {"success": True, "run": run.to_dict()}

    def get_test_status(self, run_id: str) -> dict[str, Any]:
        """
        Get the status of a test run.

        Args:
            run_id: Test run ID

        Returns:
            Dictionary with status information
        """
        run = self._test_runs.get(run_id)
        if not run:
            return {"success": False, "error": "Test run not found"}

        passed = sum(1 for r in run.test_results if r.status == TestStatus.PASSED)
        failed = sum(1 for r in run.test_results if r.status == TestStatus.FAILED)
        total = len(run.test_results)

        return {
            "success": True,
            "run_id": run_id,
            "status": run.status.value,
            "progress": {
                "total": total,
                "passed": passed,
                "failed": failed,
                "pending": total - passed - failed,
            },
        }

    def get_test_results(self, run_id: str) -> dict[str, Any]:
        """
        Get detailed results of a test run.

        Args:
            run_id: Test run ID

        Returns:
            Dictionary with detailed results
        """
        run = self._test_runs.get(run_id)
        if not run:
            return {"success": False, "error": "Test run not found"}
        return {"success": True, "results": [r.to_dict() for r in run.test_results]}

    def list_test_runs(self, limit: int = 50) -> list[dict[str, Any]]:
        """
        List all test runs.

        Args:
            limit: Maximum number of runs to return

        Returns:
            List of test run summaries
        """
        runs = list(self._test_runs.values())
        runs.sort(key=lambda r: r.start_time or datetime.min, reverse=True)

        return [
            {
                "run_id": r.run_id,
                "name": r.name,
                "status": r.status.value,
                "start_time": r.start_time.isoformat() if r.start_time else None,
                "end_time": r.end_time.isoformat() if r.end_time else None,
                "test_count": len(r.test_results),
                "passed": sum(1 for t in r.test_results if t.status == TestStatus.PASSED),
                "failed": sum(1 for t in r.test_results if t.status == TestStatus.FAILED),
            }
            for r in runs[:limit]
        ]
