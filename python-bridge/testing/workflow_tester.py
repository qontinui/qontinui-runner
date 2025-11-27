"""Workflow testing framework for automation workflows.

This module provides integration testing for automation workflows by running
them in a simulated environment. The workflows themselves ARE the tests - we
execute them against simulated GUI responses to verify they work correctly.

Key Concept:
    Traditional testing has separate "test cases" that verify "production code".
    Here, the automation workflow IS both the test case AND the production code.
    We're testing whether the workflow correctly handles different GUI responses
    by running it against historical captures from real automation runs.

    This is integration testing of the automation code itself, not of external
    applications.
"""

import logging
import time
from dataclasses import dataclass, field
from typing import Any, Set, Optional, List

try:
    from .gui_simulator import GUISimulator, SimulatedResult
except ImportError:
    from testing.gui_simulator import GUISimulator, SimulatedResult

logger = logging.getLogger(__name__)


@dataclass
class StepResult:
    """Result of executing a single workflow step in simulation.

    Each step represents one action execution within the workflow test.

    Attributes:
        step_index: Index of this step in the workflow (0-based)
        action: The action that was executed
        simulated_result: Result from the GUI simulator
        expected_states: States that should be active after this step
        actual_states: States that actually became active
        passed: Whether this step passed its verification
        error_message: Error message if step failed
    """
    step_index: int
    action: Any
    simulated_result: SimulatedResult
    expected_states: Set[str] = field(default_factory=set)
    actual_states: Set[str] = field(default_factory=set)
    passed: bool = True
    error_message: Optional[str] = None

    @property
    def states_match(self) -> bool:
        """Check if actual states match expected states."""
        if not self.expected_states:
            return True  # No expectation means any result is fine
        return self.actual_states == self.expected_states

    @property
    def action_type(self) -> str:
        """Get the type of action that was executed."""
        if isinstance(self.action, dict):
            return self.action.get('type', self.action.get('action_type', 'UNKNOWN'))
        return getattr(self.action, 'type', getattr(self.action, 'action_type', 'UNKNOWN'))


@dataclass
class WorkflowTestResult:
    """Result of executing a complete workflow test.

    Attributes:
        workflow_id: Identifier of the workflow that was tested
        workflow_name: Human-readable name of the workflow
        step_results: List of results for each step
        completed: Whether the workflow completed all steps
        failed_at: The step where failure occurred (None if completed)
        total_steps: Total number of steps in the workflow
        passed_steps: Number of steps that passed
        start_time: When test started (Unix timestamp)
        end_time: When test ended (Unix timestamp)
        initial_states: States at the start of the workflow
        final_states: States at the end of the workflow
    """
    workflow_id: str
    workflow_name: str
    step_results: List[StepResult] = field(default_factory=list)
    completed: bool = False
    failed_at: Optional[StepResult] = None
    total_steps: int = 0
    passed_steps: int = 0
    start_time: float = field(default_factory=time.time)
    end_time: Optional[float] = None
    initial_states: Set[str] = field(default_factory=set)
    final_states: Set[str] = field(default_factory=set)

    @property
    def passed(self) -> bool:
        """Check if the entire workflow test passed."""
        return self.completed and self.failed_at is None

    @property
    def duration(self) -> Optional[float]:
        """Get test duration in seconds."""
        if self.end_time is None:
            return None
        return self.end_time - self.start_time

    @property
    def pass_rate(self) -> float:
        """Get percentage of steps that passed."""
        if self.total_steps == 0:
            return 0.0
        return (self.passed_steps / self.total_steps) * 100.0


class AutomationWorkflowTester:
    """Tests automation workflows against simulated environment.

    This executes automation workflows in a simulated GUI environment to verify
    they work correctly. The workflow itself IS the test - we're not writing
    separate test cases, we're verifying that the workflow logic handles
    different GUI responses appropriately.

    Tests run immediately by default (no separate build/run steps) to provide
    fast feedback during development.

    Usage:
        # Create tester with simulator
        history = HistoricalData("captures.db")
        simulator = GUISimulator(history)
        tester = AutomationWorkflowTester(simulator)

        # Run a workflow test
        result = tester.run_workflow(my_workflow)

        if result.passed:
            print(f"Workflow passed! {result.passed_steps}/{result.total_steps} steps")
        else:
            print(f"Failed at step {result.failed_at.step_index}: {result.failed_at.error_message}")

    Attributes:
        simulator: GUISimulator for executing actions
        strict_mode: If True, any unexpected state change fails the test
    """

    def __init__(self, simulator: GUISimulator, strict_mode: bool = False):
        """Initialize workflow tester.

        Args:
            simulator: GUISimulator to use for action execution
            strict_mode: If True, enforce strict state matching (default: False)
        """
        self.simulator = simulator
        self.strict_mode = strict_mode

        logger.info(f"AutomationWorkflowTester initialized (strict_mode={strict_mode})")

    def run_workflow(self,
                    workflow: Any,
                    initial_states: Optional[Set[str]] = None) -> WorkflowTestResult:
        """Execute automation workflow in simulated environment.

        This runs the workflow step-by-step, simulating each action and
        verifying the results. The test continues until completion or failure.

        Args:
            workflow: Workflow object with actions to execute
            initial_states: Optional initial states (if None, extracted from workflow)

        Returns:
            WorkflowTestResult with complete execution details

        Note:
            Tests run immediately - no separate compile/build step needed.
            This provides fast feedback during development.
        """
        try:
            # Extract workflow metadata
            workflow_id = self._get_workflow_id(workflow)
            workflow_name = self._get_workflow_name(workflow)
            actions = self._get_workflow_actions(workflow)

            logger.info(f"Starting workflow test: {workflow_name} ({len(actions)} actions)")

            # Initialize result tracking
            result = WorkflowTestResult(
                workflow_id=workflow_id,
                workflow_name=workflow_name,
                total_steps=len(actions),
                start_time=time.time()
            )

            # Set initial states
            if initial_states is None:
                initial_states = self._get_initial_states(workflow)

            result.initial_states = set(initial_states)
            self.simulator.set_initial_states(initial_states)

            logger.debug(f"Initial states: {sorted(initial_states)}")

            # Execute each action step
            for step_index, action in enumerate(actions):
                logger.debug(f"Executing step {step_index + 1}/{len(actions)}")

                step_result = self._execute_step(
                    step_index=step_index,
                    action=action,
                    workflow=workflow
                )

                result.step_results.append(step_result)

                if step_result.passed:
                    result.passed_steps += 1
                else:
                    # Workflow failed at this step
                    result.failed_at = step_result
                    result.completed = False
                    result.end_time = time.time()
                    result.final_states = self.simulator.get_current_states()

                    logger.warning(
                        f"Workflow failed at step {step_index + 1}: "
                        f"{step_result.error_message}"
                    )
                    return result

            # All steps passed
            result.completed = True
            result.end_time = time.time()
            result.final_states = self.simulator.get_current_states()

            logger.info(
                f"Workflow test completed: {workflow_name} "
                f"({result.passed_steps}/{result.total_steps} steps passed, "
                f"{result.duration:.2f}s)"
            )
            return result

        except Exception as e:
            logger.error(f"Error running workflow test: {e}", exc_info=True)

            # Create error result
            result = WorkflowTestResult(
                workflow_id=workflow_id if 'workflow_id' in locals() else "unknown",
                workflow_name=workflow_name if 'workflow_name' in locals() else "unknown",
                completed=False,
                end_time=time.time()
            )
            return result

    def run_all_workflows(self,
                         workflows: List[Any],
                         continue_on_failure: bool = True) -> List[WorkflowTestResult]:
        """Run multiple workflows and collect results.

        This is useful for running a test suite of workflows.

        Args:
            workflows: List of workflow objects to test
            continue_on_failure: If True, continue testing after failures

        Returns:
            List of WorkflowTestResult for each workflow
        """
        results = []

        logger.info(f"Running {len(workflows)} workflow tests")

        for i, workflow in enumerate(workflows, 1):
            workflow_name = self._get_workflow_name(workflow)
            logger.info(f"Test {i}/{len(workflows)}: {workflow_name}")

            result = self.run_workflow(workflow)
            results.append(result)

            if not result.passed and not continue_on_failure:
                logger.warning(f"Stopping test suite after failure in {workflow_name}")
                break

            # Reset simulator between tests
            self.simulator.reset()

        # Summary
        passed = sum(1 for r in results if r.passed)
        logger.info(
            f"Test suite completed: {passed}/{len(results)} workflows passed"
        )

        return results

    def _execute_step(self,
                     step_index: int,
                     action: Any,
                     workflow: Any) -> StepResult:
        """Execute a single workflow step and verify results.

        Args:
            step_index: Index of this step
            action: Action to execute
            workflow: Parent workflow (for context)

        Returns:
            StepResult with execution details
        """
        try:
            # Simulate the action
            simulated_result = self.simulator.execute_action(action)

            # Check if simulation succeeded
            if not simulated_result.success:
                return StepResult(
                    step_index=step_index,
                    action=action,
                    simulated_result=simulated_result,
                    actual_states=simulated_result.resulting_states,
                    passed=False,
                    error_message=simulated_result.error
                )

            # Extract expected states (if specified in action/workflow)
            expected_states = self._get_expected_states(action, workflow)

            # Get actual resulting states
            actual_states = simulated_result.resulting_states

            # Verify states match (if expectations exist)
            passed = True
            error_message = None

            if expected_states and self.strict_mode:
                if actual_states != expected_states:
                    passed = False
                    error_message = (
                        f"State mismatch: expected {sorted(expected_states)}, "
                        f"got {sorted(actual_states)}"
                    )

            return StepResult(
                step_index=step_index,
                action=action,
                simulated_result=simulated_result,
                expected_states=expected_states,
                actual_states=actual_states,
                passed=passed,
                error_message=error_message
            )

        except Exception as e:
            logger.error(f"Error executing step {step_index}: {e}", exc_info=True)

            return StepResult(
                step_index=step_index,
                action=action,
                simulated_result=SimulatedResult(
                    success=False,
                    error=f"Execution error: {str(e)}"
                ),
                passed=False,
                error_message=f"Execution error: {str(e)}"
            )

    # Helper methods for extracting workflow data

    def _get_workflow_id(self, workflow: Any) -> str:
        """Extract workflow ID."""
        if isinstance(workflow, dict):
            return workflow.get('id', workflow.get('workflow_id', 'unknown'))
        return getattr(workflow, 'id', getattr(workflow, 'workflow_id', 'unknown'))

    def _get_workflow_name(self, workflow: Any) -> str:
        """Extract workflow name."""
        if isinstance(workflow, dict):
            return workflow.get('name', workflow.get('workflow_name', 'Unnamed Workflow'))
        return getattr(workflow, 'name', getattr(workflow, 'workflow_name', 'Unnamed Workflow'))

    def _get_workflow_actions(self, workflow: Any) -> List[Any]:
        """Extract list of actions from workflow."""
        if isinstance(workflow, dict):
            return workflow.get('actions', workflow.get('steps', []))
        if hasattr(workflow, 'actions'):
            return workflow.actions
        if hasattr(workflow, 'steps'):
            return workflow.steps
        return []

    def _get_initial_states(self, workflow: Any) -> Set[str]:
        """Extract initial states from workflow."""
        if isinstance(workflow, dict):
            initial = workflow.get('initial_states', workflow.get('start_states', set()))
            return set(initial) if initial else set()

        if hasattr(workflow, 'initial_states'):
            return set(workflow.initial_states)
        if hasattr(workflow, 'start_states'):
            return set(workflow.start_states)

        return set()

    def _get_expected_states(self, action: Any, workflow: Any) -> Set[str]:
        """Extract expected states after action (if specified)."""
        if isinstance(action, dict):
            expected = action.get('expected_states', action.get('target_states', set()))
            return set(expected) if expected else set()

        if hasattr(action, 'expected_states'):
            return set(action.expected_states)
        if hasattr(action, 'target_states'):
            return set(action.target_states)

        return set()
